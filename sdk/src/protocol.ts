export type LayerMode = "auto" | "manual";

export interface AvailableLayer {
  rid: string;
  /** Bits per second the server measured for this layer, once it has a figure. */
  bitrate_estimate: number | null;
}

/** Messages the server sends over the DataChannel. */
export type ServerMessage =
  | { type: "offer"; sdp: string }
  | {
      type: "layer_status";
      mid: string;
      available_simulcast_layers: AvailableLayer[];
      current_layer: string | null;
      target_layer: string | null;
      layer_mode: LayerMode;
    }
  | { type: "layer_changed"; mid: string; rid: string }
  // The relay protocol. Not answered yet, so the server never promotes this viewer.
  | { type: "probe_available_upload" }
  | { type: "request_offer" }
  | { type: "p2p_offer"; sdp: string }
  | { type: "p2p_answer"; sdp: string }
  | { type: "demote" };

/** Messages the SDK sends over the DataChannel. */
export type ClientMessage =
  | { type: "answer"; sdp: string }
  | { type: "set_layer"; mid: string; rid: string }
  | { type: "set_layer_mode"; mid: string; layer_mode: LayerMode }
  | { type: "perf_report"; rtt_ms: number; loss_pct: number };

export interface OfferRequest {
  type: "offer";
  sdp: string;
  room_id: string;
}

export interface OfferAnswer {
  type: "answer";
  sdp: string;
}
