# 성능과 엣지 연계 검증

이 문서는 Raspberry Pi 5에서 시뮬레이터와 `lidar-processing`을 함께 실행할 때의 입력 부하,
자원 판정과 데이터 의미 검증 방법을 정의한다. 장기 안정성과 처리 높이는 실제 실행 구성 요소의
상태와 처리 결과로 판정한다.

## 부하 입력

공개 설정의 명목 입력은 다음과 같다.

| 항목 | sensor 1대 | sensor 2대 합계 |
| --- | --- | --- |
| 회전 | 초당 10 scan | 초당 20 scan |
| 측정점 | 초당 32,000 point | 초당 64,000 point |
| 명목 scan 크기 | 회전당 약 3,200 point | 고정 계약 아님 |
| 관찰 snapshot | 해당 없음 | 기본 초당 1 record |

실제 scan 배열 길이는 회전 scheduler가 나눈 측정점 수다. Proto byte 수는 distance, angle과
quality 값의 varint 길이에 따라 달라진다.

## Image 자체 검증

ARM64 image 검증은 공개 설정, 두 sensor의 기준 측정 의미, UDS(Unix Domain Socket) 2개, 상태
진행과 정상 종료를 짧은 구간에서 검사한다.

```bash
tests/edge/verify-rust-image.sh \
  "$IMAGE_REF" \
  examples \
  "$(git rev-parse HEAD)"
```

검증 image의 `edge-validation` 명령은 제한된 표본만 메모리에 보관한다. 두 sensor에서 허공,
정적 구조와 적재면 교차, 유효 및 무효 측정과 기준 scan 변화를 확인한다. 이 명령은 실제
`lidar-processing` 연계 결과나 지속 부하 판정을 대신하지 않는다.

## 실제 엣지 연계 검증

검증 구성 요소는 4개다.

| 구성 요소 | 위치와 역할 |
| --- | --- |
| 시뮬레이터 | Raspberry Pi 5의 digest 고정 ARM64 image에서 sensor별 scan 생성 |
| `lidar-processing` | 같은 장비에서 두 UDS stream 구독과 높이 계산 |
| 처리 결과 저장 경로 | 높이와 적재율 결과의 시각 순서 및 추세 보존 |
| 상태 및 자원 수집기 | 장비에서 상태, CPU, RSS, 온도, 재시작과 OOM 표본 수집 |

관찰 stream을 사용하는 검증에서는 별도 장비의 시각화 프로그램 연결 상태도 함께 기록한다.
시각화 연결 실패는 scan 생성과 처리의 실패로 분류하지 않는다.

검증은 image digest, 공개 JSON fingerprint, seed, 환경변수 override, 처리 설정 hash, 장비 OS,
kernel, Docker 자원 제한, CPU governor와 냉각 조건을 고정한다. 실제 측정값, 사설 endpoint와
운영 로그는 Repository에 추가하지 않는다.

평균 적재 주기 86,400초는 기본 부하를 확인할 때 사용한다. 적재 및 수거 전이와 반복 추세를
확인할 때는 같은 모델을 600초 평균 적재 주기로 가속한다. 지속 시간은 검증 목적에 맞게 정한다.
전이 의미를 판정하는 실행은 filling, collecting과 다음 filling 상태를 각각 포함해야 한다.

### 자원과 생명주기

CPU는 container cgroup의 `cpu.stat` 사용 시간 차이를 실제 monotonic 표본 간격으로 나눈다.
논리 core 하나를 계속 사용하면 100 percent다. RSS(Resident Set Size)는 container cgroup의
process ID를 중복 제거한 뒤 각 `/proc/<pid>/smaps_rollup`의 `Rss`를 합산한다. cgroup v2
memory controller가 활성화된 장비에서는 `memory.current`도 별도 진단값으로 기록한다.

P95와 P99는 유효 표본을 오름차순으로 정렬한 뒤 `ceil(p * N)`번째 값을 고르는 nearest-rank
방식을 사용한다. 누락된 표본은 0으로 채우지 않는다.

판정 항목은 다음과 같다.

| 항목 | 합격 기준 |
| --- | --- |
| 시뮬레이터 CPU | P95 75 percent 이하 |
| 시뮬레이터 RSS | P95 128 MiB 이하 |
| sensor 생성률 | sensor별 명목 10 scan/s 유지 |
| 시뮬레이터 원인 sequence gap | 0 |
| 생성 상태 | sensor 2개 모두 `HEALTHY`, `sdk_errors` 0 |
| 처리 진행 | sensor별 sequence와 처리 결과 시각의 지속 증가 |
| 장비 생명주기 | OOM, container restart와 thermal throttling 0회 |

`lidar-processing`의 frame 보관 회전과 상태 counter는 해당 구성 요소의 정책이다. 연속된
simulator wire sequence가 처리 과정에서 누락되면 시뮬레이터의 게시 sequence와 처리기의 수신
sequence를 대조해 실패 영역을 분리한다.

### 데이터 의미

데이터 의미 검증은 4단계다.

1. 실행 중인 두 UDS endpoint에서 같은 시점의 원시 `ScanFrame`을 제한된 수만큼 보존한다.
2. 같은 공개 설정, seed와 시뮬레이션 시각으로 기준 scan을 재생해 wire 측정점과 정확히 비교한다.
3. 처리 설정의 강체 변환과 ROI(Region of Interest)를 원시 측정점에 독립 적용해 중앙 높이,
   P90 높이와 coverage를 계산한다.
4. 독립 계산값을 `lidar-processing` 결과와 비교하고 filling 및 collecting 구간의 높이와
   적재율 방향을 확인한다.

외부 wire `sequence` 값 `n`은 내부 모델 `scan_id` 값 `n + 1`에 대응한다. 첫 완료 scan은
실측 `scan_hz` 기준만 만들고 게시하지 않기 때문이다. 허공은 거리 0이어야 하고 적재면, 외벽과
바닥 교차는 가장 가까운 유효 거리를 가져야 한다. 처리 높이는 공개 합성 환경의 바닥 및 상단
범위 안에 있어야 하며 P90은 중앙 높이 이상이어야 한다.

시뮬레이터 기준 scan과 처리 결과는 독립적으로 판정한다. 기준 scan이 재현되지만 처리 결과가
어긋나면 처리 계약 또는 처리 구현 영역이고, 기준 scan 자체가 재현되지 않으면 시뮬레이터
영역이다. 합성 처리 설정의 `fusion_map`은 통합 검증용 항등값이므로 처리 적재율 비교는 연결과
추세의 검증이며 실제 보정 정확도의 근거가 아니다.

## 현재 검증 결과

Release 0.10.2 ARM64 image
`sha256:6208aa794b036a21ad25f03fd4c9b1b6573d25c6fbd64bd0a1cc5001f9603475`를
Raspberry Pi 5 8 GB에서 실제 `lidar-processing`과 함께 실행했다. 평균 적재 주기는 600초이고
관찰 주기는 1초다.

실행 중 sensor별 3개 frame을 수집한 결과는 다음과 같다.

| 항목 | 관측값 |
| --- | --- |
| 총 frame 및 측정점 | 6 frame, 19,200 point |
| sensor 생성 | sensor별 10 Hz, 회전당 3,200 point |
| 결정론적 wire 비교 | 19,200 point 전체 exact match |
| 잘못 인코딩된 허공 | 0 point |
| 잘못 누락된 기준 교차 | 0 point |
| `lidar_1` 기준 구성 | frame당 허공 1,600, 적재면 225, 외벽 1,375 point |
| `lidar_2` 기준 구성 | frame당 허공 2,156, 적재면 289, 외벽 755 point |
| `lidar_1` 독립 높이 | 중앙 2,733-2,751 mm, P90 3,283-3,296 mm, coverage 69-70/70 bin |
| `lidar_2` 독립 높이 | 중앙 1,638-1,645 mm, P90 1,871-1,879 mm, coverage 78/78 bin |
| 처리 ROI 유효성 | sensor별 unusable point 0 |
| 독립 적재율 비교 | 모델 0.21220, 처리 융합 0.21562 |

같은 원시 frame에 처리 좌표 변환을 독립 적용한 중앙 높이와 P90 높이는 처리 결과와 일치하는
범위였다. 이 결과는 두 UDS lane, 허공 및 외벽 표현, 적재면 변화와 처리 좌표 변환이 연결된
상태를 확인한다.

같은 image와 처리 구성으로 2시간 42분 36초 동안 795개 자원 표본을 수집했다. Docker CPU
100 percent는 논리 core 하나이고 P95는 이 문서의 nearest-rank 방식으로 계산했다.

| 항목 | 관측값 |
| --- | --- |
| 시뮬레이터 CPU | 평균 24.91 percent, P95 28.89 percent, 최대 33.81 percent |
| 시뮬레이터 RSS | 평균 6.19 MiB, P95 6.52 MiB, 최대 6.88 MiB |
| sensor 생성률 | sensor별 10.0007 scan/s |
| sensor sequence | 역행 0회, 검증 시작 이후 frame loss 증가 0회 |
| 시뮬레이터 상태 | 두 sensor 전체 표본 `HEALTHY`, `sdk_errors` 0 |
| host load average 1분 | 평균 0.55, 최대 1.22 |
| host 가용 memory | 최소 7,454.88 MiB |
| CPU 온도 | 평균 58.35 C, 최대 61.5 C |
| 생명주기 | OOM, container restart와 thermal throttling 0회 |

처리 결과 저장 경로에는 같은 시뮬레이터 instance의 결과 8,575건이 2시간 48분 동안 기록됐다.
결과 시각 간격은 평균 1.176초이고 1.5초를 넘는 공백은 없었다. 평균 적재 주기 600초에서
적재 후 수거 추세가 16회 나타났으며, 처리 적재율은 수거 직전 약 0.78-0.83에서 수거 후
약 0.01-0.03으로 내려갔다.

현재 고정한 `lidar-processing`의 `INCOMPLETE_PROFILE`, `INSUFFICIENT_SENSORS`, frame 보관 회전과
`CLOCK_UNSYNCED` 상태는 처리 구성 요소의 판정 및 보관 정책이다. 같은 구간의 시뮬레이터 sensor
sequence와 기준 scan 재현에는 대응하는 오류가 없으므로 시뮬레이터 실패로 분류하지 않는다.
