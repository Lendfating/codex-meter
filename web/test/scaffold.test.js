import assert from "node:assert/strict";
import test from "node:test";

import { frontendScaffold } from "../src/scaffold.js";

test("frontend shell is wired without business pages", () => {
  assert.deepEqual(frontendScaffold, {
    name: "codex-meter-web",
    phase: 0,
    businessPagesImplemented: false,
  });
});
