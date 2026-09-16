# 합성 환경 규격서 묶음

이 디렉토리는 공개 합성 환경의 규격서와 생성 도구를 함께 보관하는 이동 가능한 문서 경계다.
시뮬레이터 실행과 ARM64 OCI(Open Container Initiative) image는 이 디렉토리에 의존하지 않는다.

## 산출물

산출물은 6개다.

| 경로 | 역할 |
| --- | --- |
| `synthetic-environment-specification.md` | 정확한 수치, 좌표 정의와 원본 JSON을 포함한 자기완결 규격서 |
| `synthetic-environment-specification.docx` | 편집 가능한 규격서 원본 |
| `synthetic-environment-specification.pdf` | DOCX에서 렌더링한 배포용 규격서 |
| `assets/plan-view.png` | World XY 평면도 |
| `assets/isometric-view.png` | World XYZ 공간, sensor 회전면과 바닥 측정선의 입체도 |
| `SOURCE.json` | 입력 경로, SHA-256과 결합 fingerprint |

`generate.py`는 위 산출물을 만드는 도구이며 산출물 개수에 포함하지 않는다.

## 수치 정본

현재 수치 정본은 Repository의 다음 3개 JSON(JavaScript Object Notation)이다.

| 경로 | 책임 |
| --- | --- |
| `examples/environment.v1.json` | 적재 공간과 sensor 설치 |
| `examples/simulator.v2.json` | 시나리오, 측정과 seed |
| `examples/quality-profile.v1.json` | Sensor별 합성 quality 분포 |

DOCX, PDF, PNG(Portable Network Graphics)와 `SOURCE.json`은 세 입력, 문서 생성기와 문서에
사용한 적재 모델 정책의 결합 SHA-256(Secure Hash Algorithm 256-bit)을 포함한다. 수치가 다르면
JSON을 우선한다.

## 생성과 검증

DOCX와 도면 생성은 프로젝트 runtime 의존성을 바꾸지 않는 일회성 추가 package를 사용한다.

```bash
uv run --locked --group docs \
  python docs/synthetic-environment-specification/generate.py
```

PDF는 LibreOffice가 DOCX를 연 뒤 PDF로 변환한 결과다. `libreoffice` 명령이 있는 문서 작성
환경에서는 다음 명령으로 전체 산출물을 생성한다.

```bash
uv run --locked --group docs \
  python docs/synthetic-environment-specification/generate.py \
  --render-pdf
```

다음 명령은 모든 산출물이 현재 JSON fingerprint를 포함하는지 확인한다.

```bash
uv run --locked --group docs \
  python docs/synthetic-environment-specification/generate.py \
  --check
```
