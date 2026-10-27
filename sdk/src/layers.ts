import type { AvailableLayer, ClientMessage, LayerMode, ServerMessage } from "./protocol.js";

/**
 * The simulcast layers of the video track. `current` is what the server is
 * sending. `target` is what it will switch to once a keyframe of that layer
 * arrives.
 */
export interface LayerState {
  /** The video track's mid, or null while no track with layers is known. */
  mid: string | null;
  layers: AvailableLayer[];
  current: string | null;
  target: string | null;
  mode: LayerMode;
  /** Whether the server has described a track yet, layered or not. */
  described: boolean;
}

export function initialLayers(): LayerState {
  return { mid: null, layers: [], current: null, target: null, mode: "auto", described: false };
}

type LayerStatus = Extract<ServerMessage, { type: "layer_status" }>;
type LayerChanged = Extract<ServerMessage, { type: "layer_changed" }>;

/** Audio, and video from a publisher without simulcast, come with no layers. */
export function applyLayerStatus(state: LayerState, message: LayerStatus): LayerState {
  if (message.available_simulcast_layers.length === 0) {
    return { ...state, described: true };
  }

  return {
    mid: message.mid,
    layers: message.available_simulcast_layers,
    current: message.current_layer,
    target: message.target_layer,
    mode: message.layer_mode,
    described: true,
  };
}

export interface Switch {
  from: string | null;
  to: string;
}

export function applyLayerChanged(
  state: LayerState,
  message: LayerChanged,
): { state: LayerState; switched: Switch | null } {
  if (message.mid !== state.mid) return { state, switched: null };

  // In automatic mode the server retargets without saying so, which leaves a
  // target heard earlier stale. A switch always settles it.
  const settled = state.mode === "auto" || state.target === message.rid;

  return {
    state: { ...state, current: message.rid, target: settled ? null : state.target },
    switched: { from: state.current, to: message.rid },
  };
}

/**
 * The messages that ask for a layer, or for automatic selection when `rid` is
 * null. The server ignores a layer request in automatic mode, so asking for a
 * layer switches to manual first.
 */
export function requestLayer(
  state: LayerState,
  rid: string | null,
): { state: LayerState; messages: ClientMessage[] } {
  const mid = state.mid;
  if (mid === null) return { state, messages: [] };

  if (rid === null) {
    return {
      state: { ...state, mode: "auto" },
      messages: [{ type: "set_layer_mode", mid, layer_mode: "auto" }],
    };
  }

  if (!state.layers.some((layer) => layer.rid === rid)) return { state, messages: [] };

  const messages: ClientMessage[] = [];
  if (state.mode !== "manual") messages.push({ type: "set_layer_mode", mid, layer_mode: "manual" });
  messages.push({ type: "set_layer", mid, rid });

  return {
    state: { ...state, mode: "manual", target: rid === state.current ? null : rid },
    messages,
  };
}
