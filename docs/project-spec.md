# 합성 LiDAR 시뮬레이터 프로젝트 명세

## 목적

이 프로그램은 스크랩 적재 모니터링의 개발 및 통합 검증에 사용하는 합성 LiDAR(Light
Detection and Ranging) 시뮬레이터다. 공개 합성 환경에서 RPLIDAR S2E 센서 2대의 회전 단위 scan을
만들고, `ajin-edge-platform`의 `lidar-processing`이 실제 LiDAR driver와 같은 계약으로 구독할
수 있게 한다.

이 프로그램은 실제 운영 환경의 구성 요소가 아니다. 제조사 통신, 실제 센서 수집, 높이 계산,
적재율 계산과 시각화 렌더링을 구현하지 않는다.

## 범위 경계

| 포함 | 제외 |
| --- | --- |
| 공개 합성 적재 공간과 센서 2대 | 실제 현장 규격과 센서 설치값 |
| 적재 및 수거 시나리오와 2.5D 적재 표면 | 개별 스크랩 물리 충돌과 사실적 렌더링 |
| 회전 단위 scan과 합성 측정 오차 | 제조사 UDP 통신과 SDK 수신기 |
| `lidar-processing` 호환 gRPC over UDS 출력 | 높이 및 적재율 계산 로직 |
| 합성 환경용 처리 설정 exporter | 실제 운영 설정 병합과 설치 보정 |
| 적재 모델 관찰 stream과 bounded 진단 기록 | 시각화 화면, 영상 생성과 장기 기록 저장소 |
| ARM64 개발 및 검증 image | 실제 LiDAR 운영 image |

## 합성 환경과 실행 설정

공개 합성 환경의 입력은 3개다.

| 파일 | 책임 |
| --- | --- |
| `examples/environment.v1.json` | 적재 공간, 바닥, 상단과 센서 위치 및 방향 |
| `examples/simulator.v2.json` | 적재 시나리오, 측정, 관찰 전송, 진단과 seed |
| `examples/quality-profile.v1.json` | 센서별 합성 quality 분포 |

환경과 품질 설정은 `lidar_1`, `lidar_2`를 정확히 한 번씩 포함한다. 환경 경계는 자기 교차가
없는 다각형이고 바닥은 상단보다 낮다. 센서의 0도 및 90도 방향은 길이가 1인 직교 단위벡터다.
세 파일의 sensor ID와 참조 관계가 다르면 시뮬레이터는 시작하지 않는다.

합성 모델, 센서, 측정, quality와 seed는 versioned JSON으로 관리한다. 파일 및 UDS(Unix Domain
Socket) 경로, 실행 식별자, 평균 적재 주기, 수거 임계치 중심값, 관찰 endpoint와 진단 override는
CLI(Command-Line Interface) 인자 또는 환경변수로 제공한다. CLI 인자, 환경변수, JSON과 코드
기본값 순서로 값을 선택한다.

공간과 센서 정의는 scan frame에 포함하지 않는다. `lidar-processing`은 시뮬레이터의 환경 JSON을
직접 읽지 않으며 합성 처리 설정 exporter가 같은 환경을 외부 처리 형식으로 변환한다.

## 적재와 수거 시나리오

적재 표면은 투입구 주변의 국소 부피 증가, 요철과 안식각 기반 경사 이완으로 변한다. 적재물은
투입구에 고정된 종 모양을 유지하지 않고 주변의 낮은 위치로 확산된다. 두 투입구는 설정된 활성
비율과 주변 높이 차이에 따라 전환된다.

평균 적재 시간, 회차별 적재 시간 및 속도, 수거 임계치와 수거 속도는 설정으로 조정한다. 수거는
기존 표면을 점진적으로 낮추며 완료 시 전체 표면을 기준 바닥으로 되돌린 뒤 다음 회차를 시작한다.
표면은 항상 바닥과 외벽 상단 사이에 유지되고 부피 변화는 동일한 설정과 seed에서 재현된다.

## 센서 측정

공개 측정 프로파일은 센서당 초당 32,000 sample, 초당 10회전과 0.05-30 m 측정 범위를 사용한다.
명목상 한 회전은 3,200개 측정점이지만 scan 배열 길이는 고정 계약이 아니다. 시뮬레이터는 sample
시각과 회전 경계에 포함된 실제 측정점 수를 사용하며 두 센서는 서로 기다리지 않고 독립적으로
회전을 완료한다.

각 광선은 현재 적재 표면, 바닥, 외벽과 고정 표면 중 가장 가까운 교차점을 측정한다. 교차점이
없거나 측정 범위 밖이면 거리 0으로 표현한다. 거리 0은 높이 0이 아니라 무효 거리다.

시뮬레이터는 합성 실수값을 SDK(Software Development Kit) HQ 각도 Q14와 거리 Q2로 양자화한 뒤
`ajin-edge-platform` driver와 같은 정수 변환을 적용한다. 외부 측정점은 각도 millidegree, 거리
millimeter와 0-63 quality로 구성하고 각도 오름차순으로 정렬한다.

기준 측정과 합성 오차는 분리한다. 합성 오차는 거리 noise, 무효 측정, 낙하물, 빈틈, 수거 가림,
반사 경로 오류와 sensor dropout을 포함한다. 설정과 seed가 같으면 같은 시뮬레이션 시각의 표면과
측정점 내용이 같다. wall clock, process instance와 공개 sequence는 실행 생명주기를 따른다.

## Scan 구독 계약

시뮬레이터는 같은 process에서 sensor별 gRPC(Google Remote Procedure Call) server-streaming UDS
endpoint 2개를 제공한다. `lidar-processing`은 각 endpoint의
`LidarScanSource.SubscribeScans`를 호출하는 구독자다. 계약 정본과 고정 출처는
`contracts/lidar/v1/`에 둔다.

`ScanFrame`은 다음 11개 필드를 제공한다.

| 필드 | 의미 |
| --- | --- |
| `schema_version` | scan 계약 version |
| `edge_id` | 개발 및 검증 실행의 edge 식별자 |
| `sensor_id` | `lidar_1` 또는 `lidar_2` |
| `sequence` | sensor instance 안에서 1부터 증가하는 공개 scan 순번 |
| `acquired_at_unix_ms` | scan 완료 wall clock 시각 |
| `acquired_monotonic_ns` | scan 완료 monotonic 시각 |
| `sdk_status` | 정상 합성 scan의 `OK` 상태 |
| `scan_hz` | 직전 완료 scan과 현재 완료 scan의 간격으로 계산한 회전 빈도 |
| `samples` | 정규화된 측정점 배열 |
| `instance_id` | process 시작마다 sensor별로 생성하는 UUID |
| `config_revision` | 개발 및 검증 설정 식별자 |

첫 완료 scan은 `scan_hz` 계산 기준으로만 사용하고 게시하지 않는다. 각 sensor endpoint는 최신
frame 2개만 보관하고 동시 구독자 8개까지 받는다. 느린 구독자는 오래된 frame을 잃을 수 있으며
같은 `instance_id`의 `sequence` 간격으로 손실을 확인한다. scan 단위 ACK(Acknowledgement), 재전송과
중복 처리 계약은 사용하지 않는다.

구독자의 연결, 해제와 재연결은 scan 생성과 적재 시나리오를 초기화하지 않는다. 시뮬레이터 process
재시작은 빈 적재 공간, 새 sensor별 `instance_id`와 `sequence` 1로 시작한다. 구독 요청 오류와
상태 표현은 고정한 `ajin-edge-platform` 계약을 따른다.

시뮬레이터는 `lidar-driver-a`와 `lidar-driver-b` 호환 상태 파일을 별도 상태 directory에 기록한다.
상태 파일은 첫 frame 게시 전 `STARTING`, 게시 후 `HEALTHY`를 나타낸다.

## 합성 처리 설정

합성 처리 설정 exporter는 공개 환경의 센서 위치와 방향을 `lidar-processing`의 강체 변환,
50 mm 단면 ROI(Region of Interest), 높이 범위와 측정 필터로 변환한다. 출력에는 처리기가 요구하는
항등 `fusion_map`, 동일한 sensor 가중치와 `demo: true` calibration을 포함한다. 이 값은 통합
실행을 위한 중립적인 합성 fixture이며 적재율 정확도나 실제 보정을 나타내지 않는다.

exporter는 실제 edge 설정을 입력받거나 병합하지 않는다. 실제 센서 설치 보정, 융합 보정과
적재율 계산은 이 Repository의 책임이 아니다. 합성 실행의 `site_id`, `edge_id`와
`config_revision`은 시뮬레이터와 처리 설정에서 같은 값을 사용한다.

## 적재 모델 관찰

시뮬레이터는 scan 계약과 별도로 적재 모델 관찰 데이터를 JSON Lines TCP(Transmission Control
Protocol) stream으로 보낸다. 연결할 때 공개 합성 공간, 외벽, 투입구와 센서 정보를 포함한 정적
header를 한 번 보내고 기본 1초마다 시뮬레이션 시각, 적재율, 상태와 현재 표면을 포함한 동적
snapshot을 보낸다.

publisher는 최신 대기 snapshot 1개만 보관한다. 관찰 수신기 연결 실패와 느린 처리는 scan 생성,
gRPC 구독과 적재 시나리오를 중단하지 않는다. 재연결하면 정적 header부터 다시 보낸다.

실시간 3D 표시, 관찰 기록과 MP4 생성은 별도 시각화 프로그램이 수행한다. 시뮬레이터 image는 3D
renderer, FFmpeg와 시각화 개발 의존성을 포함하지 않는다.

## 진단 기록

진단 기록은 기준 광선 교차, 최종 측정과 당시 적재 모델 상태를 sensor별 제한된 scan 수만큼
JSON Lines 파일로 남기는 개발 검증 기능이다. 진단은 설정으로 비활성화할 수 있고 일반 scan
구독이나 관찰 stream을 대체하지 않는다.

## 개발 환경과 배포

| 항목 | 적용 기준 |
| --- | --- |
| 기본 실행 구현 | Rust 1.96 |
| 계약 및 문서 자동화 | Python 3.14, uv |
| 배포 산출물 | Linux ARM64 단일 platform OCI image |
| 실행 대상 | Raspberry Pi 5 개발 및 통합 검증 환경 |

Release image는 정적으로 link한 Rust 실행 파일과 필수 라이선스 고지만 포함한다. Python runtime,
실제 센서 SDK, compiler, uv, 3D renderer, FFmpeg와 테스트 의존성은 포함하지 않는다. 엣지 장비는
GitHub Container Registry에 게시된 image를 digest로 받아 실행하며 image를 직접 빌드하지 않는다.

## 보안과 공개 범위

공개 합성 공간과 센서 규격은 프로젝트용 합성값이며 코드, fixture, 문서, 그림과 영상에 사용할
수 있다. 실제 센서 측정값, 품질 관측 원본, 운영 로그, 사설 주소, 자격 증명과 실제 현장 정보는
공개 설정, 테스트 고정값, 로그와 image에 포함하지 않는다.

현재 외부 계약에는 자격 증명이 없다. 장비별 endpoint와 실행 경로는 Git에 추적하지 않는 환경
파일로 관리한다.

## 완료 기준

- 동일한 설정, seed와 시뮬레이션 시각에서 적재 표면, 측정점과 관찰 snapshot 내용이 재현된다.
- 빈 상태, 적재, 투입구 전환, 수거와 수거 완료 상태가 자동 검증된다.
- 표면 확산, 부피 보존, 외벽, 바닥, 허공과 합성 오차가 자동 검증된다.
- RPLIDAR S2E 공개 프로파일과 `ajin-edge-platform` SDK 이후 정수 변환이 자동 검증된다.
- sensor별 회전 경계, 독립 생성, 가변 배열 길이와 첫 scan 생략이 자동 검증된다.
- gRPC UDS 구독, latest-two 손실 정책, 재연결과 상태 파일이 자동 검증된다.
- 관찰 연결 실패와 느린 수신기가 scan 생성과 구독을 막지 않는다.
- 합성 처리 설정과 두 sensor frame을 실제 `lidar-processing`이 `GOOD` 상태로 수락한다.
- Linux ARM64 image가 빌드되고 digest 기준 반복 검증 절차를 제공한다.
- Raspberry Pi 5에서 Release image와 `lidar-processing`을 함께 실행하여 scan 및 높이 의미,
  자원 여유, 생명주기와 지속 안정성을 검증한다.
- 고정한 S2E SDK와 외부 driver source의 frame 의미 및 정수 변환을 자동 검증한다.
