# Stream

str0m 기반 WebRTC SFU 스트리밍 서버

## Architecture

```
┌──────────────────────────────────────┐
│            Rust Servers              │
│                                      │
│  ┌───────────┐  ┌────────────────┐  │
│  │ Axum HTTP  │  │ Axum HTTPS     │  │
│  │ (내부 통신) │  │ (SDP Offer)    │  │
│  └─────┬──────┘  └──────┬────────┘  │
│        │   tokio threads │          │
│  ──────┴─────────────────┴───────── │
│  ┌────────────────────────────────┐ │
│  │   SFU (std thread)             │ │
│  │   UDP Socket Main Loop         │ │
│  │   str0m + ICE                  │ │
│  │   Data Channel                 │ │
│  └────────────────────────────────┘ │
└──────────────────────────────────────┘
```

## SFU 서버

- **str0m** 기반 WebRTC SFU
- UDP 소켓 메인루프 — std thread
- Axum HTTP (NestJS 통신용) — tokio thread
- Axum HTTPS (WebRTC 초기 SDP Offer) — tokio thread
- Data Channel: SDP 재협상, 클라이언트 성능 측정, P2P 연결 준비

### 내부 API (Axum HTTP)

| Endpoint      | 설명                                  |
| ------------- | ------------------------------------- |
| `create_room` | 방 생성                               |
| `delete_room` | 방 삭제                               |
| `get_room`    | 시청자 수 등 Rust 서버 전용 정보 조회 |

### 스레드 간 상태 공유

`rooms`를 웹서버 스레드와 UDP 메인루프 스레드가 공유 → `Arc<Mutex<_>>` 사용

## P2P Relay

SFU 대역폭 비용 절감을 위한 P2P Relay 모드.

```
기본:  SFU ──► Client 1
       SFU ──► Client 2

P2P:   SFU ──► Relay Node (Client 1) ──► Client 2
```

### 흐름

1. Data Channel로 클라이언트 성능 측정 (업로드/다운로드 속도, 안정성)
2. 지표가 안정적이면 P2P Relay 노드로 지정
3. Data Channel로 P2P 연결 준비 및 사전 작업
4. P2P 연결 수립
5. Relay 노드 품질 저하 또는 연결 끊김 시 SFU 폴백

## 기술 선택

| 선택             | 이유                                                                               |
| ---------------- | ---------------------------------------------------------------------------------- |
| **Rust**         | GC 없음 → 레이턴시 예측 가능                                                       |
| **str0m**        | webrtc.rs 대비 네트워크 파이프라인을 조립식으로 구성 → 커스텀 파이프라인 추가 용이 |
| **SFU (vs CDN)** | CDN은 레이턴시가 길어 실시간 방송에 부적합                                         |

## Roadmap

- [ ] WHIP 프로토콜 지원 (OBS 연동)
- [ ] gRPC 도입 (NestJS ↔ Axum 통신 레이턴시 단축)
- [ ] 다시보기 지원 (FFmpeg 파이프라인)
- [ ] 방송 화면 캡쳐 API (썸네일용)

## Open Questions

- Data Channel 메시지 우선순위 정책
- P2P Relay 노드 선정 기준 고도화
- SFU 폴백 시 레이턴시 최소화
- `Arc<Mutex<_>>` 성능 개선 (lock contention)
