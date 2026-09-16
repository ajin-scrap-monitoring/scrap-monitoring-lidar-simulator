# 개발 계획

## 현재 상태

현재 제품 경계는 5개다.

| 경계 | 상태 | 정본 |
| --- | --- | --- |
| 공개 합성 환경과 적재 시나리오 | 구현 완료 | `examples/`, `docs/configuration.md` |
| RPLIDAR S2E 호환 scan 생성 | 구현 완료 | `docs/sdk-compatibility.md` |
| `lidar-processing` scan 출력 | 실제 gRPC over UDS 계약 검증 완료 | `contracts/lidar/v1/`, `edge-platform-integration/` |
| 적재 모델 관찰 stream | 구현 완료 | `docs/observation.md` |
| ARM64 image 배포 | Rust 단일 실행 파일 image와 자동 검증 | `docs/deployment.md` |

시뮬레이터는 하나의 적재 모델에서 정확히 2개 sensor scan을 만들고 sensor별
gRPC(Google Remote Procedure Call) over UDS(Unix Domain Socket) endpoint를 제공한다.
`lidar-processing`은 시뮬레이터의 환경 JSON을 직접 읽지 않으며 exporter가 같은 공개 합성 환경에서
처리 설정을 만든다.

실행 구현은 Rust `src/` 하나다. Python은 계약 검사와 합성 환경 문서 생성 자동화에만 사용한다.
ARM64 OCI(Open Container Initiative) image에는 정적으로 link한 Rust 실행 파일과 필수 라이선스
고지만 포함한다.

Raspberry Pi 5 연계 검증은 두 sensor의 기준 광선 구성, scan sequence, 처리 높이와 적재율 추세,
시뮬레이터 자원 및 생명주기를 실제 `lidar-processing` 실행 결과와 함께 확인했다. 조건과 관측값은
[`performance.md`](performance.md)가 정본이다.

## 채택한 구조

구조 결정은 10개다.

| 항목 | 결정 |
| --- | --- |
| Repository | 현재 이력, 계약과 Release를 유지하는 기존 Repository |
| 배포 단위 | Rust 실행 파일 하나를 포함한 `linux/arm64` OCI image |
| 내부 공간 정본 | 오른손 World XYZ, meter, degree |
| 센서 scan 경계 | Sensor polar, millimeter, millidegree |
| 처리 좌표 경계 | Sensor별 Section XZ, millimeter |
| 좌표 변환 위치 | World 정본에서 처리 설정을 만드는 exporter 경계 |
| Scan 전송 | sensor별 server-streaming gRPC over UDS endpoint 2개 |
| 관찰 전송 | JSON Lines TCP version 1, 기본 1초, latest-one 비차단 출력 |
| 시뮬레이션 상태 | 하나의 coordinator와 event 구간별 불변 snapshot |
| 동시성 | 제어 및 I/O runtime과 sensor별 고정 계산 worker 2개 |

World XYZ와 Section XZ는 서로 다른 책임을 가진다. Section XZ는 `lidar-processing`이 sensor별 높이
계산에 사용하는 2D 투영이라 World Y 정보를 보존하지 않는다. Exporter가 공개 합성 환경의 sensor
위치와 방향을 처리 좌표로 변환한다.

외부 version 1 계약은 scan Proto, sensor ID, UDS 파일 이름, 상태 경로, 환경변수 이름,
observation record와 sequence 의미를 고정한다. 공개 schema ID에 포함된 기존 경로도 version 1
계약 식별자로 유지한다.

## 검증 구조

자동 검증은 5개 계층이다.

| 계층 | 검증 범위 |
| --- | --- |
| Rust 정적 검사 | rustfmt와 기본 및 전체 feature Clippy |
| Rust 테스트 | 설정, 수치 모델, wire, 외부 출력과 생명주기 |
| Python 자동화 테스트 | JSON Schema, 고정 처리 계약과 문서 생성기 |
| Live 계약 검사 | 실제 Rust process, 두 UDS lane과 고정한 `ProcessingEngine` |
| ARM64 image 검사 | platform, source revision, image 내용과 외부 출력 smoke test |

CI(Continuous Integration)는 위 계층과 Markdown, 합성 환경 산출물, 제3자 라이선스 고지를 함께
검사한다. Live 계약 검사는 release profile binary를 시작하고 exporter 출력, sensor별 UDS 구독,
상태 파일, 관찰 연결, 종료 정리와 `lidar-processing` 결과를 확인한다.

## 완료 기준

### 기능 합격

- 공개 JSON과 환경변수 우선순위 및 검증 결과가 일치한다.
- 두 sensor가 각각 10 Hz scan과 초당 32,000개의 명목 sample을 생성한다.
- 외부 Proto field, SDK 정수 변환, 첫 scan 생략과 완료 시각 의미가 일치한다.
- Sensor별 sequence, instance, latest-two와 구독 오류 동작이 계약과 일치한다.
- 상태 파일의 `STARTING`, `HEALTHY`, service 경로와 원자 교체 동작이 일치한다.
- Observation header, snapshot, 1초 주기와 latest-one 장애 격리 동작이 일치한다.
- `lidar-processing` loader와 `ProcessingEngine`이 두 sensor 결과를 `GOOD`으로 판정한다.
- 기준 scan이 허공, 정적 구조와 적재면 교차 및 시간 변화를 포함한다.
- 같은 model version, 설정과 seed의 반복 실행 결과가 일치한다.

### Release 전 단기 합격

Raspberry Pi 5 8 GB에서 시뮬레이터, `lidar-processing`과 상태 수집 process를 함께 실행한다. Docker
CPU 100 percent는 논리 core 하나로 해석한다.

| 항목 | 합격선 |
| --- | --- |
| 지속 시간 | 준비 2초 이상, 측정 30초 이상 |
| 시뮬레이터 CPU | 측정 구간 P95 75 percent 이하 |
| 시뮬레이터 RSS | P95 128 MiB 이하 |
| 두 sensor frame 생성 지연 | P99 70 ms 이하 |
| 시뮬레이터 원인 sequence gap | 0 |
| 처리 상태 | 두 sensor와 융합 결과 `GOOD` 유지 |
| 데이터 의미 | 생성 scan과 처리 높이 및 적재율 검증 통과 |
| 장비 상태 | OOM, container restart와 thermal throttling 0회 |

## 다음 갱신 조건

다음 외부 계약 갱신 조건은 1개다.

1. 교체될 처리 구성 요소의 계약이 고정되면 `SOURCE.json`, Proto, exporter와 직접 호환 검사를 함께 갱신한다.

처리 구성 요소가 교체되기 전에는 현재 처리기의 보관 정책, 상태 counter와 계산 결과를 simulator
결함으로 판정하지 않는다. 처리 호환 자료는 새 계약 갱신 전까지 현재 고정 version의 재현
자료로 유지한다.

## 설정과 변경 원칙

시뮬레이터는 `simulator.v2.json`, `environment.v1.json`과 `quality-profile.v1.json`을 읽는다. 난수
stream과 수치 모델 재현 경계는 simulation model version 1이다. 합성 안식각 35도와 경사 이완
반복 상한은 engine 정책이며 배포 override나 환경 형상 입력이 아니다.

외부 Proto 또는 처리 설정이 바뀌면 고정 source commit, 로컬 Proto, exporter, 인계 묶음과 직접
호환 검증을 같은 변경에서 갱신한다. 합성 환경이 바뀌면 공개 JSON 정본과 파생 규격서를 함께
갱신한다. `docs/project-spec.md`와 기존 `docs/internal/**`은 사용자의 명시적 요청 없이 수정하지
않는다.

변경은 Organization 개발 운영 규칙의 Issue, branch, Pull Request와 Release 절차를 따른다. Issue와
Pull Request 제목 summary는 명사구로 끝낸다.
