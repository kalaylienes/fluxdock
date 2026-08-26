import { readFileSync } from "node:fs";

import { expect, test } from "@playwright/test";

/**
 * The README images are checked into the repository, so nothing stops a stale
 * or wrongly produced one being committed. This is the guard: it reads the PNG
 * headers only, needs no browser and no image library, and runs in the ordinary
 * suite.
 *
 * Regenerate with `npm run docs:shots`, which asserts far more than this does
 * and refuses to write a file that fails.
 */

const EXPECTED = [
  ["docs/media/floating-dark.png", 900, 378],
  ["docs/media/taskbar.png", 1140, 132],
  ["docs/media/states.png", 900, 378],
] as const;

test("the README images are RGBA at the size the layout produces", () => {
  for (const [file, width, height] of EXPECTED) {
    const bytes = readFileSync(file);
    expect(bytes.subarray(0, 8).toString("hex"), file).toBe("89504e470d0a1a0a");
    expect(bytes.readUInt32BE(16), `${file} width`).toBe(width);
    expect(bytes.readUInt32BE(20), `${file} height`).toBe(height);
    // Colour type 6 is RGBA. Type 2 is RGB, which is what a screenshot taken
    // without omitBackground produces, and it is why the rounded corners used
    // to bleed white on GitHub.
    expect(bytes[25], `${file} colour type`).toBe(6);
  }
});
