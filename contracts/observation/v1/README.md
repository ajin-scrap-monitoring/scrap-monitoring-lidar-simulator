# 적재 모델 관찰 계약 버전 1

## 전송 경계

전송에는 시뮬레이터 producer와 시각화 receiver의 2개 구성 요소가 참여한다. 시뮬레이터는 TCP
(Transmission Control Protocol) client로 receiver에 연결하고, receiver는 설정한 주소와
port에서 server로 대기한다.

시뮬레이터는 UTF-8 JSON 객체 하나와 LF(Line Feed) 1 byte를 한 레코드로 전송한다. 한
레코드는 LF를 포함하여 최대 1,048,576 byte다. receiver는 TCP packet 경계가 아니라 LF를 기준으로
레코드를 조립해야 하며, 연결 종료 시 남은 불완전 레코드를 폐기해야 한다. receiver는
시뮬레이터에 어떤 byte도 보내지 않는다.

`header.schema.json`과 `observation.schema.json`이 레코드 형식의 정본이다. TCP 연결의
첫 레코드는 `load_model_stream_header`이고 이후 레코드는 `load_model_observation`이다.
연결이 바뀌면 시뮬레이터는 header를 다시 전송한다. version 1 파일은 호환성을 깨는 방식으로
변경하지 않으며 field 의미나 framing이 달라지면 새 version을 추가한다.

## 전달 의미

시뮬레이터는 한 실행에서 전송 대상으로 선택한 레코드에 1부터 증가하는 `sequence`를
부여한다. TCP 연결이 바뀌어도 같은 `run_id`의 sequence는 계속 증가한다. receiver가 같은
`run_id`에서 sequence의 증가 폭이 1보다 큰 레코드를 받으면 중간 상태가 producer에서
폐기되었거나 연결 중 유실된 것으로 처리한다.

전송은 ACK(Acknowledgement)가 없는 best-effort 방식이다. producer는 최신 대기 observation
1개만 유지하고 이전 대기 레코드를 재전송하지 않는다. receiver는 중복 없는 전달이나 모든
중간 상태의 전달을 가정하지 않는다. TCP 연결 안의 완전한 레코드 순서는 sequence 순서와
같다.

## 좌표와 장면

header의 `scene.coordinate_system`은 오른손 좌표계이며 z축이 위쪽이다. 모든 거리와 좌표의
단위는 meter이고 각도 단위는 degree다. `boundary_xy_m`은 적재 공간의 수평 경계
polygon이고 `floor_z_m`과 `top_z_m`은 바닥과 외벽 상단의 절대 z 좌표다.

sensor의 `p0_m`은 원점이고 `u0`과 `u90`은 서로 직교하는 단위 방향 벡터다. degree 단위
각도 `angle_deg`를 radian으로 변환한 `angle_rad`를 사용하여 광선 방향을
`cos(angle_rad) * u0 + sin(angle_rad) * u90`으로 계산한다. 관찰 renderer는 sensor
위치와 기본 방향을 표시하는 데 이 값을 사용한다.

`inlet_positions_xy_m`의 배열 순서는 `scenario.current_inlet_index`가 참조하는 순서다.
`current_inlet_index`는 filling 상태에서 현재 투입구를 가리키며 collecting 상태에서는
null이다.

## 적재 표면

`surface.heights_m[y_index][x_index]`는 `x_coordinates_m[x_index]`와
`y_coordinates_m[y_index]`가 만나는 격자 node의 절대 z 좌표다. 좌표 배열은 엄격히
증가하고 `heights_m` shape은 y 좌표 수와 x 좌표 수의 순서다. 격자는 경계 polygon의
bounding box를 덮으므로 polygon 밖의 node가 포함될 수 있다. renderer는 표면을
`scene.boundary_xy_m` 안으로 제한한다.

`scenario.elapsed_s`는 실행 시작 이후의 시뮬레이션 시각이다.
`scenario.surface_updated_at_s`는 현재 높이 값에 반영된 마지막 모델 갱신 시각이다.
`surface_fill_ratio`는 바닥과 상단 사이 전체 용량에 대한 현재 부피 비율이고,
`surface_volume_m3`는 바닥 위 현재 부피다.

## 실행 식별

`run_id`는 시뮬레이터 process 실행을 구분하며 header와 모든 observation에 들어간다.
`environment_id`는 사용한 환경 설정을 식별한다. `seed`와 `input_fingerprint_sha256`은
기록 묶음의 입력 동일성을 확인하는 값이며 receiver는 fingerprint를 opaque lowercase
SHA-256(Secure Hash Algorithm 256-bit) 값으로 취급한다.

`fixtures/observation.v1.jsonl`은 header 1개와 observation 1개로 구성한 공개 합성
fixture다. 실제 장비 주소, 운영 로그와 실제 sensor 측정값은 계약 fixture에 포함하지
않는다.
