# 적재 모델 관찰 스트리밍

## 목적

시뮬레이터는 기존 scan 전송과 별도로 현재 적재물 표면 모양과 시나리오 상태를 개발 장비에
계속 전송한다. 일반 scan 데이터로 표면을 재구성하지 않고 시뮬레이터 내부의 읽기 전용
snapshot을 사용한다.

## 실행 경계

관찰 publisher는 시뮬레이터와 함께 항상 실행한다. 기본 주기는 시뮬레이션 시각 1초이며
전송 시각에만 현재 적재물 표면을 복사한다. 수신기 연결 실패, 느린 수신기와 관찰 전송
오류는 scan 생성 및 기존 전송을 중단하거나 지연시키지 않는다.

publisher는 전송 대기 중인 최신 레코드 1개만 보관한다. 새 레코드가 들어오면 아직
전송하지 않은 이전 상태를 폐기한다. 연결이 끊어지면 제한된 지수 증가 대기 뒤에
재연결하고 과거 상태를 누적하거나 재전송하지 않는다. 엣지 장비는 관찰 파일과 영상을
생성하지 않는다.

시뮬레이터 image에는 관찰 JSON Lines(JavaScript Object Notation Lines) producer만 포함한다.
FFmpeg와 3D rendering 코드 및 의존성은 포함하지 않는다.

## 전송 계약

관찰 stream은 2개 레코드 형식을 순서대로 사용한다.

1. `load_model_stream_header`: 연결 직후 1회 전송하는 실행 식별 정보, 경계, 바닥, 외벽
   상단, 투입구와 2개 sensor 위치 및 방향.
2. `load_model_observation`: 기본 1초마다 전송하는 sequence, 시뮬레이션 시각, 적재율,
   부피, filling 또는 collecting 상태와 현재 적재물 표면 격자.

wire 형식, 전달 의미와 field 의미는
[`contracts/observation/v1/`](../contracts/observation/v1/)이 정본이다. 기존 scan 계약과
수신 프로그램에는 관찰 field를 추가하지 않는다.

## 실행

시뮬레이터는 관찰 수신 endpoint를 명시적으로 받는다.

| 인자 | 기본값 | 의미 |
| --- | --- | --- |
| `--observation-host` | 필수 | 관찰 수신기의 TCP host |
| `--observation-port` | 필수 | 관찰 수신기의 TCP port |
| `--observation-interval-s` | `1.0` | 시뮬레이션 초 기준 전송 간격 |

```bash
cargo run --locked -- run \
  --config /path/to/simulator.v2.json \
  --grpc-socket-dir /run/lidar \
  --status-dir /status \
  --site-id example-site \
  --edge-id example-edge \
  --config-revision example-r1 \
  --deployment-revision example-deployment-r1 \
  --observation-host observation-receiver-host \
  --observation-port 17000
```

관찰 간격은 0보다 크고 86,400초 이하여야 한다. 첫 레코드는 처음 완료한 scan 시점의
현재 상태이며 이후 설정 간격을 지난 첫 scan 완료 시점의 상태를 전송한다. 시간 사이의
표면을 보간하지 않는다.

종료 집계의 `sent`는 TCP writer가 운영체제에 전달한 observation 수이며 receiver 처리를
보장하지 않는다. `dropped`는 latest-only 교체, 전송 실패, 크기 상한 또는 종료 때문에
폐기한 레코드 수다. `connection_failures`는 연결 또는 연결 사용 실패 횟수다.

## 시각화 프로그램 경계

실시간 표시, bounded 관찰 기록, 기록 재생과 MP4 생성은 별도 개발 장비의 별도
Repository가 담당한다. 생성된 JSON Lines, frame과 MP4는 기본적으로 Git에 추적하지
않는다. 구현 요구사항은 [`visualizer-requirements.md`](visualizer-requirements.md)에 둔다.

실제 sensor 측정값, 품질 관측 원본, 운영 로그, 사설 주소와 자격 증명은 관찰 fixture와
공개 문서에 포함하지 않는다. 관찰 stream에는 시뮬레이터 실행에 제공된 장면 설정이 들어가므로
운영 network와 기록 파일의 접근 범위를 별도로 통제한다.
