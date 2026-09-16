# 적재 모델 시각화 프로그램 요구사항

## 범위

시각화 프로그램은 별도 개발 장비에서 관찰 stream을 수신하고 현재 적재 모델을 공학용
3D 화면과 MP4로 표현한다. 개별 scrap 조각의 물리 충돌이나 사실적 rendering은 범위에
포함하지 않는다. 라이다 시뮬레이터, scan 수신 프로그램과 높이 계산 process는 이
Repository의 구현 대상이 아니다.

시각화 프로그램은 다음 5개 구성 요소로 나눈다.

1. TCP 관찰 수신기.
2. version 1 레코드 parser와 validator.
3. bounded live 상태 및 기록 저장소.
4. 적재 공간과 표면 mesh renderer.
5. live preview, 기록 재생과 MP4용 CLI(Command-Line Interface).

## 입력 인터페이스

수신기는 [`contracts/observation/v1/`](../contracts/observation/v1/)의 version 1 계약을
구현한다. 수신기는 TCP server이고 시뮬레이터가 client다. packet 분할과 여러 레코드의 packet
병합을 허용하며 LF 단위로 JSON 레코드를 복원한다. 연결의 첫 레코드로
`load_model_stream_header` 1개를 받고 이후 `load_model_observation`을 계속 받는다. 최대
line 크기 1,048,576 byte를 넘는 입력과 알 수 없는 version 또는 type을 거부한다.

수신기는 `run_id`, `environment_id`, `seed`와 `input_fingerprint_sha256`로 실행 묶음을
구분한다. 같은 실행에서 sequence gap을 발견하면 누락 상태를 기록하되 연결과 live 표시를
계속한다. 이전 sequence, 중복 sequence와 감소한 시뮬레이션 시각은 해당 레코드를
거부한다. 새 `run_id`는 새 실행으로 전환한다.

시각화 프로그램은 일반 scan stream을 입력으로 사용하거나 scan point로 적재 표면을
추정하지 않는다. header가 장면을 제공하고 observation이 현재 적재물 표면을 제공하므로
시뮬레이터 설정 파일은 필수 입력이 아니다.

유효한 header는 LiDAR 2대의 위치와 방향을 정확히 포함한다. renderer는 두 센서를 모두 표시하고 `sensor_id`로 구분한다.

## 실시간 동작

live 경로는 수신, 기록과 rendering 사이에 bounded queue를 사용한다. 느린 renderer는
시뮬레이터 연결에서 읽기를 무기한 막지 않으며, 화면 갱신 대기 상태는 오래된 frame을
폐기하고 최신 상태를 유지한다. 연결이 끊기면 화면에 disconnected 상태와 마지막 정상
레코드의 시각을 표시하고 같은 port에서 다음 연결을 계속 기다린다.

기본 화면은 고정 사선 camera에서 전체 경계, 바닥, 외벽, 투입구, sensor 위치와 방향,
현재 적재물 표면을 표시한다. 상면 camera도 선택할 수 있어야 한다. 화면 overlay는
`elapsed_s`, `surface_fill_ratio`, `phase`, `cycle_index`, `sequence`와 연결 상태를
표시한다. filling 상태에서는 현재 투입구를 구분해 표시한다.

## mesh 의미

renderer는 `heights_m[y][x]`를 같은 index의 x, y 좌표와 결합하여 표면 vertex를 만든다.
인접한 격자 node는 결정론적인 대각선 규칙으로 삼각형 2개를 만든다. 경계 polygon 밖의
표면은 표시하지 않으며, concave polygon도 처리한다. 바닥은 경계 polygon을 삼각분할하고
외벽은 각 경계 edge의 바닥부터 `top_z_m`까지 만든다.

동일한 유효 레코드와 camera 및 출력 설정은 같은 mesh vertex, face, frame 선택 순서를
생성해야 한다. floating-point 표시 형식과 색상 차이 때문에 pixel 전체를 고정하는
snapshot test는 사용하지 않는다.

## 기록과 재생

기록 기능은 header부터 수신한 원본 JSON line을 순서대로 보존한다. 기록은 명시적으로 켠
경우에만 수행하며 byte 또는 record 수 기준의 상한을 필수로 받는다. 상한에 도달하면
기록을 정상 종료하고 live 표시는 계속한다. 불완전 line과 무효 레코드는 기록하지 않는다.

재생은 저장된 레코드의 시뮬레이션 시각을 기준으로 구간을 선택한다. 영상 frame 시각에는
그 시각보다 늦지 않은 가장 최근 레코드를 사용한다. 사용자가 영상 재생 시간과 시간
가속률을 동시에 지정하면 오류로 종료한다.

## CLI 요구사항

CLI는 live와 replay의 2개 mode를 제공한다.

| mode | 필수 입력 | 선택 입력 | 출력 |
| --- | --- | --- | --- |
| live | listen host, port | camera, 기록 경로와 상한 | 실시간 preview, 선택적 JSON Lines 기록 |
| replay | JSON Lines 입력 | 시작 및 종료 시각, duration 또는 time scale, FPS, 해상도, camera | preview 또는 MP4 |

기본 camera는 전체 공간이 보이는 고정 사선 시점이고 `top`을 추가로 제공한다. MP4 출력은
명시한 경로만 사용한다. frame, 관찰 기록과 영상 파일은 기본 `.gitignore` 대상이다.
rendering 및 FFmpeg 의존성은 시각화 Repository에만 두며 엣지 시뮬레이터 image에 추가하지
않는다.

## 자원과 오류 처리

수신 line, live queue, 기록량, frame 수, FPS(Frame Per Second)와 해상도에 각각 유한한
상한을 둔다. 초기 기본 상한은 최대 3,000 frame, 60 FPS와 3,840 x 2,160 해상도다.
receiver 또는 renderer 장애는 엣지 시뮬레이터의 scan 생성 및 기존 scan 전송 상태를
변경하지 않는다.

TCP version 1은 개발용 단일 producer 연결만 다루며 ACK, 인증, 압축, broker와 전달
보장을 제공하지 않는다. 저장한 관찰 파일에는 장면 정보가 포함된다.

## 자동 검증

자동 검증은 다음 8개 범주를 포함한다.

1. 계약 fixture와 schema validation.
2. 분할, 병합, 불완전 line과 최대 line 크기 처리.
3. version, type, 필수 field와 알 수 없는 field 오류.
4. sequence gap, 중복, 역순과 run 전환 처리.
5. y-major 표면 mesh, 경계 clipping과 concave polygon 처리.
6. 동일 입력의 mesh 및 frame 선택 결정론.
7. queue, 기록, frame, FPS와 해상도 상한.
8. live 및 replay CLI 상호 배타 option과 오류 종료 code.

공개 통합 fixture는
[`contracts/observation/v1/fixtures/observation.v1.jsonl`](../contracts/observation/v1/fixtures/observation.v1.jsonl)을
사용한다. 시각적 회귀는 구조 및 수치 속성을 검증하고 pixel 전체 snapshot을 사용하지
않는다.
