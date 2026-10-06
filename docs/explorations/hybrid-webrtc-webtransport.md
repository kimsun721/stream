# Hybrid WebRTC + WebTransport 구상

> 실험적 아이디어

## 동기

WebRTC와 WebTransport의 트레이드오프가 분명함:

|               | WebRTC              | WebTransport                |
| ------------- | ------------------- | --------------------------- |
| 지연          | 초저지연            | 낮음 (WebRTC보단 살짝 높음) |
| 핸드쉐이크    | 복잡 (SDP/ICE/DTLS) | HTTP 기반, 단순             |
| 네트워크 변경 | ICE 재협상 → 버퍼링 | QUIC 기반 → 끊김 적음       |
| CDN           | 사실상 불가         | 가능                        |
| 트래픽 비용   | 비쌈                | CDN 활용 시 저렴            |
| 생태계        | 성숙                | 미성숙                      |

WebTransport가 미성숙하다는 단점 하나 빼면 거의 다 우위. 다만 그 하나가 큼 — 브라우저 호환성, 운영 안정성이 아직.

## 아이디어

스트리머는 WebRTC, viewer는 **브라우저 지원 여부에 따라 WebTransport / WebRTC 분기**.

```
streamer ──WebRTC──▶ SFU ─┬─WebRTC──▶ viewer (legacy 브라우저)
                           └─WebTransport──▶ viewer (지원 브라우저)
```

장점:

- 스트리머는 WebRTC 호환성 + 초저지연 송출 그대로
- 지원 viewer는 WebTransport 안정성 + CDN 활용 가능성
- 미지원 viewer는 폴백 — 누구도 잃지 않음

트레이드 오프:

- 어느정도 초저지연성을 포기
- 프론트에서 미디어 데이터를 렌더링하는 코드를 직접 구현해야 함
- SFU에서 RTP -> QUIC 변환으로 인해 변환 비용 발생
- 코드내에서 WebTransport와 WebRTC 스택이 다른 두 코드가 공존. 스택 통합 운영 복잡도 증가.
- 실험적임. 브라우저들은 거의 다 WebTransport를 지원하지만 서버측 라이브러리 자체가 성숙하지 않음

## 우선순위

모든걸 구현하고 나서 안정화가 되고나서 실험적으로 적용 또는 아래 조건 충족 시 실험적 도입:

- 트래픽 비용이 P2P relay만으로 해결 안 되는 규모로 커진다 (CDN 활용 욕구)

## 관련 결정

- [ADR 0003](../decisions/0003-receive-simulcast-layers-from-streamer.md) — simulcast는 WebTransport 분기에서도 layer 선택 로직 재활용 가능. layer를 raw bytes로 어떻게 컨테이너화할지가 추가 결정 포인트.
- P2P relay ([p2p-relay.md](../feature/p2p-relay.md)) — WebRTC viewer끼리만 P2P 가능. WebTransport viewer는 server fan-out 그대로 받음. 그래서 도입해도 P2P 보완재이지 대체재 아님.

## 메모

- str0m + wtransport 같은 멀티 스택 SFU 사례를 어느정도 찾아보기.
- low-latency CMAF/LL-HLS 도입과의 경계도 모호 — WebTransport와 LL-HLS는 비슷한 자리 노림. 셋 중 어느 쪽이 정착할지 한 번 더 보고 판단.
