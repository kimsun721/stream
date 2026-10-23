import assert from "node:assert/strict";
import { test } from "node:test";

import {
  applyLayerChanged,
  applyLayerStatus,
  initialLayers,
  requestLayer,
  type LayerState,
} from "../src/layers.ts";

const layered = (overrides: Partial<LayerState> = {}): LayerState => ({
  mid: "1",
  layers: [
    { rid: "l", bitrate_estimate: 300_000 },
    { rid: "h", bitrate_estimate: 2_500_000 },
  ],
  current: "l",
  target: null,
  mode: "auto",
  described: true,
  ...overrides,
});

const status = (layers: string[]) => ({
  type: "layer_status" as const,
  mid: "1",
  available_simulcast_layers: layers.map((rid) => ({ rid, bitrate_estimate: null })),
  current_layer: null,
  target_layer: "l",
  layer_mode: "auto" as const,
});

test("a track without layers is described but offers no choice", () => {
  const state = applyLayerStatus(initialLayers(), status([]));
  assert.equal(state.described, true);
  assert.equal(state.mid, null);
});

test("a layered track takes the server's view", () => {
  const state = applyLayerStatus(initialLayers(), status(["l", "m", "h"]));
  assert.equal(state.mid, "1");
  assert.deepEqual(
    state.layers.map((layer) => layer.rid),
    ["l", "m", "h"],
  );
  assert.equal(state.target, "l");
});

test("a switch in auto mode settles any target", () => {
  const { state, switched } = applyLayerChanged(layered({ target: "h" }), {
    type: "layer_changed",
    mid: "1",
    rid: "m",
  });
  assert.deepEqual(switched, { from: "l", to: "m" });
  assert.equal(state.current, "m");
  assert.equal(state.target, null);
});

test("in manual mode a switch to another layer keeps the pending target", () => {
  const { state } = applyLayerChanged(layered({ mode: "manual", target: "h" }), {
    type: "layer_changed",
    mid: "1",
    rid: "m",
  });
  assert.equal(state.target, "h");
});

test("a switch on another track is ignored", () => {
  const before = layered();
  const { state, switched } = applyLayerChanged(before, { type: "layer_changed", mid: "2", rid: "h" });
  assert.equal(switched, null);
  assert.equal(state, before);
});

test("asking for a layer in auto mode goes manual first", () => {
  const { state, messages } = requestLayer(layered(), "h");
  assert.deepEqual(messages, [
    { type: "set_layer_mode", mid: "1", layer_mode: "manual" },
    { type: "set_layer", mid: "1", rid: "h" },
  ]);
  assert.equal(state.mode, "manual");
  assert.equal(state.target, "h");
});

test("asking for the current layer in manual mode sets no target", () => {
  const { state, messages } = requestLayer(layered({ mode: "manual" }), "l");
  assert.deepEqual(messages, [{ type: "set_layer", mid: "1", rid: "l" }]);
  assert.equal(state.target, null);
});

test("asking for auto sends the mode alone", () => {
  const { state, messages } = requestLayer(layered({ mode: "manual" }), null);
  assert.deepEqual(messages, [{ type: "set_layer_mode", mid: "1", layer_mode: "auto" }]);
  assert.equal(state.mode, "auto");
});

test("nothing is asked for an unknown layer or before any track", () => {
  assert.deepEqual(requestLayer(layered(), "x").messages, []);
  assert.deepEqual(requestLayer(initialLayers(), "h").messages, []);
});
