# Rust simulation model version 1

## 적용 범위

이 문서는 Rust model version 1의 stream seed, 난수 sampling, smooth rate profile, 적재 시나리오
event와 LiDAR 측정 결정론 계약의 정본이다. `randomness`, `rate_profile`, `scenario`와
`measurement`는 network 출력과 독립적인 library 경계다. 공개 JSON version과 simulation model
version은 별개이며 기존 version 2 입력 schema를 변경하지 않는다.

계약의 구성 요소는 seed 파생, PRNG(Pseudorandom Number Generator), sampling, rate profile,
시나리오 event, sensor 회전 및 HQ 변환과 최종 측정의 7개다. 난수 word, sampling과 생성된 segment의
bit pattern은 exact vector로 검증한다. Scripted rate, 시나리오, 좌표, 왜곡과 SDK 변환은
`tests/fixtures/model-v1/`의 고정 기준값과 비교한다.

## Stream seed

`derive_stream_seed(root_seed, scope, domain)`은 아래 byte열의 SHA-256(Secure Hash Algorithm
256-bit)을 계산한다. 정수는 모두 unsigned big-endian이고 문자열은 정규화하지 않은 UTF-8이다.
`d`와 `s`는 문자 수가 아니라 각각 domain과 sensor identifier의 byte 수다.

| 시작 offset | 길이 | 값 |
| --- | --- | --- |
| 0 | 16 | ASCII `scrap-lidar-sim`과 NUL 1 byte |
| 16 | 4 | model version `1` |
| 20 | 8 | root seed |
| 28 | 1 | global scope `0`, sensor scope `1` |
| 29 | 4 | domain 길이 `d` |
| 33 | `d` | domain bytes |
| `33 + d` | 4 | sensor identifier 길이 `s` |
| `37 + d` | `s` | sensor identifier bytes |

Domain은 비어 있지 않은 현상별 이름이다. Global scope의 sensor identifier는 길이 0이며 sensor
scope에서는 비어 있지 않아야 한다. 각 문자열 길이는 `u32` 범위다. 길이 prefix와 scope tag는
문자열 연결 위치, NUL 포함 문자열과 global/sensor 이름의 충돌을 방지한다.

호출부는 현상별 domain마다 독립적인 `ModelRng`를 소유한다. Global `fill-rate-profile`과 sensor
`distance-noise`는 exact seed vector로 고정한다. 다른 stream을 만들거나 소비해도 기존 stream의
상태는 변하지 않는다. Sensor 회전의 기존 deterministic hash 초기 각도는 이 PRNG 경계가 아니다.

## PRNG와 sampling

`ModelRng`는 [xoshiro256 star-star 1.0](https://prng.di.unimi.it/xoshiro256starstar.c)의 256-bit
상태 전이를 사용한다. 공식 참조 구현은 public domain이다. 구현은 기존 `sha2` crate 외의 난수
의존성을 추가하지 않는다. 이 난수는 합성 시뮬레이션 전용이며 자격 증명이나 보안 token에 사용하지
않는다.

Digest의 byte `0..7`, `8..15`, `16..23`, `24..31`을 각각 big-endian `u64`로 읽어 상태 word
`s0..s3`를 만든다. 모든 word가 0이면 `s0`만 1로 바꾼다. 상태 전이와 곱셈은 modulo `2^64`를
사용하며 한 호출은 다음 순서로 실행한다.

```text
output = rotl64(s1 * 5, 7) * 9
t = s1 << 17
s2 = s2 xor s0
s3 = s3 xor s1
s1 = s1 xor s2
s0 = s0 xor s3
s2 = s2 xor t
s3 = rotl64(s3, 45)
return output
```

Sampling 규칙은 3개다.

| API | 규칙 |
| --- | --- |
| `unit_f64` | word의 상위 53 bit를 `f64`로 변환하고 `2^-53`을 곱한 `[0, 1)` 값 |
| `boolean` | 새 word의 최상위 bit가 1이면 true |
| `uniform(lower, upper)` | 별도 곱셈과 덧셈으로 `lower + (upper - lower) * unit` 계산 |

각 유효한 sampling 호출은 word 1개를 소비한다. Uniform의 두 경계가 같아도 word를 소비하고
lower를 그대로 반환한다. 경계는 유한하고 순서가 맞으며 차이가 유한해야 한다. 잘못된 범위는
난수를 소비하지 않고 오류를 반환한다. 서로 다른 경계에서 반올림 결과가 upper에 도달하면
`upper.next_down()`을 반환하므로 범위는 `[lower, upper)`다. Fused multiply-add는 사용하지 않는다.

## Smooth rate profile

`SmoothRateSegment`는 시작 시각, 길이와 deviation으로 구성한다. 시작 시각은 유한한 0 이상의
값이고 길이는 유한한 양수이며 deviation은 유한하고 -1보다 크다. Segment 외부와 양 끝의 factor는
1이다. 내부 normalized 시각 `u`의 계산은 다음과 같다.

```text
factor = 1 + deviation * sin(pi * u)^2
deviation_integral = deviation * duration * (u / 2 - sin(2 * pi * u) / (4 * pi))
```

적분의 `u`는 `[0, 1]`로 제한한다. Profile은 정렬된 segment와 누적 deviation 적분을 보관하며
시작과 종료 시각 각각의 단조 증가 순서를 tolerance 없이 검증한다. 같은 경계 시각은 허용한다.
경계 검색은 해당 시각 이하인 항목 수를 사용한다. Segment 사이와 마지막 남은 구간은 factor 1이다.
전체 종료 시각까지의 적분은 누적 오차와 무관하게 profile duration 자체를 반환한다.

내부 tolerance는 `max(1, duration_s) * 1e-12`다. Segment 겹침, 마지막 종료 경계와 전체 deviation
적분의 0 판정에 이 값을 사용한다. 조회 시각은 tolerance 없이 `[0, duration_s]`를 요구하고
역방향 적분을 거부한다. 일반 fixture 비교 tolerance를 내부 상태 검증에 대신 사용하지 않는다.

시뮬레이터는 잔여 길이가 최소 change duration의 2배 이상인 동안 segment 쌍을 추가한다. 각 쌍은
첫 길이 uniform, 둘째 길이 uniform, 양수 편차의 선행 여부 boolean, balance uniform 순서로 word
4개를 소비한다. Balance 상한은 양수/음수 허용 편차와 해당 길이의 곱 중 작은 값이다. 두 편차는
각각 `balance / positive_duration`과 `-balance / negative_duration`이므로 쌍의 적분은 상쇄된다.
Cursor는 `cursor + (first_duration + second_duration)` 순서로 갱신한다.

Factor 범위 한쪽이 정확히 1이거나 segment 쌍을 만들 시간이 없으면 난수를 소비하지 않고 빈
profile을 반환한다. 생성과 직접 구성 모두 segment 최대 100,000개를 허용한다. 유한한 종료 시각을
만들 수 없거나 양수 길이를 더해도 float 시각이 진행하지 않으면 오류를 반환한다.

## 시나리오 난수 stream

시나리오 난수 stream은 4개다.

| Domain | Scope | 소비 순서 |
| --- | --- | --- |
| `cycle-plan` | global | fill 길이 factor, 수거 기준, collection 길이 factor |
| `fill-rate-profile` | global | fill profile의 segment 쌍 |
| `collection-rate-profile` | global | collection profile의 segment 쌍 |
| `surface-roughness` | global | fill event의 peak, radius와 중심 후보 |

각 fill plan은 `cycle-plan`에서 길이 factor와 수거 기준을 순서대로 소비한다. Fill 종료 시 collection
plan은 같은 stream에서 길이 factor를 소비한다. Collection 종료 후 다음 cycle이 같은 순서를 반복한다.
Fill과 collection rate profile은 서로 다른 stream을 사용하므로 한 phase의 segment 수가 다른 phase의
난수열을 바꾸지 않는다.

각 fill surface event는 `surface-roughness`에서 peak를 먼저 소비한다. Peak가 0이면 해당 event는
추가 word를 소비하지 않는다. Peak가 0이 아니면 radius를 소비하고 최대 32개 중심 후보마다 거리와
각도 word를 하나씩 소비한다. 첫 번째 경계 내부 후보를 사용하며 모두 실패하면 현재 투입구 위치를
사용한다.

## 시나리오 event

표면 갱신 시각은 시작점 0에 대한 전역 index `i * surface_update_interval_s`로 계산한다. 현재 phase
종료 시각과 다음 표면 갱신 시각 중 이른 값을 다음 event로 사용한다. 반복 덧셈으로 갱신 시각을
만들지 않는다.

Event 처리는 5단계다.

1. 직전 표면 갱신 시각부터 event까지 rate profile 적분으로 부피를 계산한다.
2. Fill이면 현재 투입구에 부피와 선택적 국소 요철을 적용하고 collection이면 전체 표면에서 부피를 제거한다.
3. 합성 안식각 기준으로 표면 경사를 이완한다.
4. 표면 갱신 시각을 event 시각으로 기록한다.
5. Phase 종료 event이면 상태를 전환하고 아니면 fill 투입구 전환 조건을 평가한다.

Fill 종료 event는 누적 오차 대신 목표 부피와 현재 부피의 차이를 적용한다. Collection 종료 event는
남은 부피 전체를 제거한다. Fill 평균 속도는 목표 부피를 fill 길이로 나눈 값이고 collection 평균
속도는 시작 부피를 collection 길이로 나눈 값이다. Collection 길이는 mean fill duration과 collection
길이 factor의 곱이다.

투입구 전환은 fill ratio가 activation ratio에 도달한 뒤 평가한다. 각 투입구 주변의 면적 가중 평균
높이를 비교하며 최저 높이 후보가 같은 경우 작은 index를 사용한다. 현재 위치가 후보보다 높고 그
차이가 설정 기준 이상일 때만 전환한다. Collection 상태에는 현재 투입구가 없다.

24시간인 86,400초를 mean fill duration의 reference로 사용한다. 이 비율은 fill 및 collection rate
change duration 범위에만 곱한다. 표면 update interval과 collection duration factor에는 적용하지
않는다.

## Snapshot과 상한

`advance_to`는 목표 시각까지 발생한 모든 event를 시간순으로 반환한다. 각 event record는 phase 전환과
투입구 평가까지 끝난 AFTER-event 상태와 불변 표면 snapshot을 가진다. Event와 같은 시각의 sensor
sample은 이 새 snapshot을 사용하고 더 이른 sample은 직전 snapshot을 사용한다. 정적 grid는 `Arc`로
공유하고 높이 배열은 event마다 분리한다.

한 번의 `advance_to`가 반환하는 event 상한은 4,096개이고 event별 높이 배열의 합계 상한은 16 MiB다.
실제 event 상한은 두 값과 현재 grid node 수로 계산하며 초과 요청은 simulator 상태를 변경하지 않고
실패한다. 경계 다각형 상한은 256개 vertex, 투입구 상한은 64개, 높이장 node 상한은 262,144개다.
경사 이완의 공개 호출 상한은 256회다. Index overflow, 유한한 다음 시각을
만들 수 없는 phase와 표면 schedule, 적용하지 못한 계획 부피는 오류다. 같은 model version, 설정과
seed를 사용한 실행은 같은 지원 architecture에서 동일한 event 상태와 표면을 만든다.

## Sensor 회전과 HQ 변환

Sensor별 초기 각도는 model RNG와 독립적인 SHA-256 값이다. Payload는 ASCII
`sensor-rotation`과 NUL 1 byte, big-endian `u64` root seed, 정규화하지 않은 sensor identifier
UTF-8 bytes를 순서대로 연결한다. Digest를 unsigned 256-bit 정수로 해석하고 360을 곱한 뒤
`2^256`으로 나눈 각도를 HQ Q14 tick으로 반올림한다.

회전 scheduler는 sample rate와 rotation rate의 입력 decimal 값을 정수 비율로 보존한다. Rotation
종료까지의 누적 sample 수는 비율의 올림값이며 sample index는 scan 경계에서 초기화하지 않는다.
따라서 sample rate 5 Hz와 rotation rate 2 Hz의 연속 scan 길이는 `[3, 2, 3, 2]`이고 sample 시각은
전역 sample index를 sample rate로 나눈 연속 시각이다. Sample 시각은 rotation 시작 이상이고 완료
시각 미만이다. `lidar-processing` 입력 계약에 따라 한 rotation의 최대 sample 수는 32,768개다.
`ceil(sample_rate_hz / rotation_rate_hz)`가 이 상한을 넘는 설정은 scheduler 생성 전에 거부한다.
Decimal 비율과 rotation 완료 시각은 임의 정밀도 정수로 계산한 뒤 `f64`에 한 번만 반올림하며
부동소수점 나눗셈으로 대체하지 않는다. Scan ID와 sample index는 signed 64-bit 양수 범위를 넘기
전에 오류를 반환하고, 해당 index의 완료 시각이 유한한 `f64` 범위를 벗어나면 오류를 반환한다.

각도와 거리는 half-up 방식으로 각각 HQ Q14와 HQ Q2 tick에 양자화한다. 광선과 정적 교차 cache는
입력 배열 위치가 아니라 양자화된 실제 HQ angle tick을 key로 사용한다. Millidegree 변환의 곱셈은
`u64`에서 수행하며 같은 millidegree로 변환된 sample은 입력 순서를 유지하는 안정 정렬을 사용한다.

## 측정 난수 stream

측정 난수 stream은 7개다.

| Scope | Domain | 책임 |
| --- | --- | --- |
| Global | `falling-material` | 적재 중 낙하물 event |
| Global | `voids` | 적재 표면 빈틈 event |
| Global | `collection-occlusion` | 수거 중 가림 event |
| Sensor | `reflection-error` | 반사 경로 오류 선택과 거리 감소 |
| Sensor | `dropout` | dropout 간격과 지속 시간 |
| Sensor | `distance-noise` | 절단 정규분포 거리 noise |
| Sensor | `quality` | 유효 및 무효 sample quality |

각 stream은 다른 현상의 활성화 여부와 소비량에 영향을 받지 않는다. 공간 event는 두 sensor가
공유하는 simulation timeline에서 한 번 생성하고 sensor별 측정기는 같은 event history를 읽는다.
모든 event 수명과 dropout 구간은 `[start, end)`다. Event 시각의 표면 sample은 event 이후의 새
snapshot을 사용한다.

반사 오류가 활성화되면 sample 수만큼 선택 word를 먼저 소비하고 같은 수만큼 거리 감소 word를
소비한다. Dropout scheduler는 최초 간격, 각 event의 지속 시간, 다음 간격 순서로 소비한다. 거리
noise는 sample별 rejection sampling을 사용한다. Noise 표준편차나 제한이 0이면 noise word를
소비하지 않는다. Quality는 최종 거리의 유효 여부와 관계없이 sample마다 word 1개를 소비한다.
Quality 빈도 누적합과 합계는 checked `u128`로 계산한다. 각 누적 경계는
`ceil(cumulative * 2^64 / total)`의 정수값이며 raw `u64` word가 이 경계보다 작을 때 해당 quality를
선택한다. 이 방식은 sample마다 word 1개를 소비하고 각 누적 확률을 최대 `1 / 2^64`만큼 위로
양자화한다.

## 측정 처리 순서

한 sample의 최종 측정 처리 순서는 8단계다.

1. 표면, 바닥, 외벽과 고정 표면의 기준 광선 교차 및 공간 왜곡.
2. 반사 경로 오류의 거리 감소.
3. Dropout의 거리 0 대입.
4. 절단 정규분포 거리 noise 적용.
5. 음수 거리의 0 clamp.
6. HQ Q2 거리 양자화.
7. 최소 및 최대 측정 거리 유효성 판정.
8. 유효 또는 무효 분포의 quality 선택.

낙하물과 수거 가림은 기존 표면보다 가까운 후보만 선택한다. 빈틈은 기존 표면보다 먼 후보만
선택하되 같은 광선의 정적 교차 거리와 최대 측정 거리를 넘지 않는다. 바닥과 빈 동적 표면처럼
후보 거리가 같으면 먼저 평가한 정적 후보를 유지한다.

기준 scan은 광선 및 정적 교차 cache의 장면과 최소 및 최대 측정 거리 metadata를 공유한다. 공간
왜곡은 이 장면과 거리 범위가 시뮬레이터 설정과 정확히 일치할 때만 적용한다. 각 공개 event resolver는
적용 가능한 동적 표면 sample이 없어도 입력 event 전체를 먼저 검증한다.

ScanFrame factory는 sample 정규화 전체가 성공한 뒤 monotonic clock과 wall clock을 읽는다. 첫
완료 scan은 sensor별 scan rate 기준만 만들고 frame을 내보내지 않는다. 이후 frame은 sensor별
완료 monotonic 시각 차이로 scan rate를 계산하고 sequence를 1부터 증가시킨다. UUID(Universally
Unique Identifier)와 두 clock source는 fixture에서 주입할 수 있다.

Dropout scheduler와 sensor별 측정기는 한 호출이 실패하면 해당 호출에서 소비한 난수와 진행 상태를
복원한다. 같은 입력을 수정해 재시도한 결과는 실패 호출 없이 실행한 같은 seed의 결과와 같다.

같은 model version, 설정과 seed는 같은 지원 architecture에서 동일한 난수 소비, profile, 공간
event와 sensor별 측정을 만든다. `sin`을 포함한 rate 조회와 적분은 model version 1 fixture의
tolerance로 검증한다. 기존 word 전이, seed layout, sampling 소비량, profile 생성, event 생성이나
측정 처리 순서를 바꾸면 새 model version이 필요하다.
