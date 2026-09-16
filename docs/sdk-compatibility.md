# RPLIDAR SDK 출력 정합성

## 적용 경계

정합성 기준은 2개 구현을 함께 고정한다.

| 기준 | 역할 |
| --- | --- |
| [Slamtec RPLIDAR SDK 2.1.0 commit](https://github.com/Slamtec/rplidar_sdk/tree/99478e5fb90de3b4a6db0080acacd373f8b36869) | HQ node 자료형과 장비 API |
| `contracts/lidar/v1/upstream.json`의 `ajin-edge-platform` commit | SDK 호출, 정수 변환, 시각과 gRPC 계약 |

시뮬레이터는 제조사 UDP packet과 SDK 수신기를 구현하지 않는다. 장면의 합성 측정값을 HQ node가
표현할 수 있는 값으로 양자화한 뒤 `ajin-edge-platform` driver가 SDK 결과에 적용하는 변환을 그대로 수행한다.

## 장비 mode와 생성 빈도

`ajin-edge-platform` driver는 S2E에 UDP channel로 연결하고 `getTypicalScanMode`가 반환한 mode를
`startScan(false, true, 0, &used_mode)`로 시작한다. scan mode와 motor speed를 별도로
덮어쓰지 않는다.

공개 생성 설정은 초당 32,000 sample, 초당 10회전과 0.05-30m 거리를 사용한다. 명목상
센서당 회전당 3,200 point다. 이 값은 생성 프로파일의 빈도 비율이며 SDK 반환 배열이나
`ScanFrame.samples`의 고정 길이 계약이 아니다.

## 외부 driver의 SDK 수집 순서

`ajin-edge-platform` driver의 한 회전 처리 순서는 5단계다.

1. `grabScanDataHq`로 HQ node 배열 수집.
2. `ascendScanData`로 각도 오름차순 정렬.
3. scan 완료 wall clock과 monotonic clock 기록.
4. HQ node별 각도, 거리와 quality 정수 변환.
5. 첫 scan을 `scan_hz` 계산 기준으로만 사용하고 다음 scan부터 frame 게시.

SDK 배열의 첫 node 시각을 요청하는 `grabScanDataHqWithTimeStamp`는 `ajin-edge-platform`에서 사용하지
않는다. 외부 frame 시각은 첫 측정점 시각이 아니라 수집 완료 시각이다. 시뮬레이터도 이 의미를
따른다.

`ascendScanData` 결과는 각도 오름차순이다. 시뮬레이터는 HQ 양자화 뒤 `angle_mdeg`를 안정
정렬한다. 같은 정수 각도를 가진 측정점은 생성 순서를 유지한다. 소비자는 첫 각도를 회전의
고정 0도로 가정하지 않고 배열이 비어 있지 않거나 길이가 고정이라고 가정하지 않는다.

## HQ node와 ScanSample 변환

SDK HQ node의 주요 필드는 `angle_z_q14`, `dist_mm_q2`, `quality`, `flag`다. `ajin-edge-platform` driver는
다음 정수식을 사용한다.

```text
angle_mdeg = ((angle_z_q14 * 90000 + 8192) / 16384) % 360000
distance_mm = dist_mm_q2 / 4
quality = quality_byte >> 2
```

나눗셈은 정수 나눗셈이다. HQ 각도는 한 회전 65,536단계이고 HQ 거리는 1 mm당 4단계다.
시뮬레이터는 합성 실수 각도와 거리를 가장 가까운 HQ 값으로 먼저 양자화한 뒤 위 변환을 적용한다.
거리 0은 `distance_mm == 0`으로 유지된다.

외부 `quality`는 SDK의 원래 8-bit byte가 아니다. `ajin-edge-platform` driver가 상위 6 bit로 정규화한
0-63 값이다. 시뮬레이터의 quality profile은 양자화 전 HQ byte 분포를 정의하고 wire 변환에서
오른쪽으로 2 bit 이동한다. 거리 유효 여부와 quality 값은 서로 다른 필드이며 소비자는
`distance_mm > 0`과 quality filter를 각각 적용한다.

## Frame 시각과 순서

외부 계약 호환 frame은 scan 완료 시각을 다음 필드로 함께 기록한다.

| 필드 | clock과 단위 |
| --- | --- |
| `acquired_at_unix_ms` | realtime Unix millisecond |
| `acquired_monotonic_ns` | monotonic nanosecond |

`scan_hz`는 같은 sensor의 현재 완료 monotonic 시각과 직전 완료 monotonic 시각의 차이로
계산한다. 첫 완료 scan은 직전 시각이 없으므로 게시하지 않는다. 그다음 frame부터 sensor별
`sequence`를 1부터 증가시킨다.

시뮬레이터는 실행 시작마다 sensor별 새 `instance_id`를 만든다. process 재시작은 sequence를 1로
되돌리지만 새 `instance_id`로 이전 실행과 구분한다. 구독 중 frame 유실은 같은 instance의
sequence gap으로 확인한다.

## 시뮬레이터가 재현하는 범위

시뮬레이터가 재현하는 항목은 다음과 같다.

- 공개 프로파일의 명목 sample 및 회전 빈도
- sensor별 독립 회전과 가변 길이 scan
- 적재 표면, 바닥, 외벽과 허공의 광선 교차 결과
- HQ Q14 각도와 Q2 거리 양자화
- 외부 driver 정수 변환과 angle 정렬
- 완료 시각, 첫 scan 생략, scan rate와 instance sequence 의미
- 외부 Proto의 필드와 gRPC 구독 경계

시뮬레이터가 재현하지 않는 항목은 다음과 같다.

- 제조사 UDP packet과 capsule decode
- 장비별 motor 변동과 실제 측정점 개수 분포
- 실제 환경의 무효 거리와 quality 분포
- SDK 호출 지연과 네트워크 수신 병합
- 장비 serial, firmware 상태와 사설 endpoint

재현하지 않는 관측이 필요하면 실제 장비 자료를 `docs/internal/`의 별도 산출물로 보관한다.
실제 측정값을 공개 기본값이나 fixture로 복제하지 않는다.

## 의존성과 검증

시뮬레이터는 RPLIDAR SDK에 link하지 않는다. SDK source, compiler와 build 산출물도 시뮬레이터 image에
포함하지 않는다. 외부 SDK는 BSD-2-Clause license를 따른다.

자동 검증은 HQ 표현 가능성, 정수 변환 경계, 0 거리, quality 이동, 안정 angle 정렬, 첫 scan
생략과 sensor별 sequence를 Rust 단위 및 통합 테스트에서 확인한다. Live 계약 검증은 실제 `run`
process의 두 gRPC stream과 exporter 출력을 고정한 `lidar-processing`의 `ProcessingEngine`에 넣는다.

```bash
cargo build --release --locked --bin scrap-monitoring-lidar-simulator
uv run --locked --group integration \
  python -m tools.verify_rust_runtime_contract \
  --runtime-binary target/release/scrap-monitoring-lidar-simulator \
  --edge-platform-root /path/to/ajin-edge-platform
```

검증은 release profile binary를 요구하며 정확한 판정 범위는
[`../edge-platform-integration/`](../edge-platform-integration/)이 정본이다.
