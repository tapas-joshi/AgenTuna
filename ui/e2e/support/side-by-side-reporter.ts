import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
  type Dirent,
} from "node:fs";
import { basename, dirname, join, relative } from "node:path";
import { deflateSync, inflateSync } from "node:zlib";

import type { FullConfig, FullResult, Reporter } from "@playwright/test";

/**
 * TU-025: a failed screenshot comparison leaves `-expected.png`, `-actual.png` and `-diff.png` in
 * the Playwright output directory. This reporter merges whichever of them exist into a single
 * `-side-by-side.png` under `test-results/side-by-side/`, so the CI artifact shows the golden,
 * the render and the diff in one image instead of three separate files.
 *
 * The merge runs only when a comparison failed; a green run pays a directory scan. PNG support is
 * deliberately limited to what a Chromium screenshot produces (8-bit, non-interlaced, grey, RGB or
 * RGBA) and uses `node:zlib`, so the suite needs no extra dependency.
 */

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const ACTUAL_SUFFIX = "-actual.png";
const EXPECTED_SUFFIX = "-expected.png";
const DIFF_SUFFIX = "-diff.png";
/** Pixels of mid-grey drawn between panels so the golden/render boundary is visible. */
const GUTTER_WIDTH = 2;
const GUTTER = [0x7f, 0x7f, 0x84, 0xff] as const;
const CHANNELS: Record<number, number> = { 0: 1, 2: 3, 4: 2, 6: 4 };

/** A decoded image: RGBA, 8 bits per channel, `width * height * 4` bytes. */
export interface RgbaImage {
  readonly width: number;
  readonly height: number;
  readonly pixels: Buffer;
}

/** A failed comparison: the golden, the render and the diff that Playwright wrote out. */
interface FailureArtifacts {
  readonly label: string;
  readonly expected?: string;
  readonly actual: string;
  readonly diff?: string;
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let index = 0; index < 256; index += 1) {
    let value = index;
    for (let bit = 0; bit < 8; bit += 1) {
      value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    }
    table[index] = value >>> 0;
  }
  return table;
})();

function crc32(bytes: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc = CRC_TABLE[(crc ^ byte) & 0xff]! ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type: string, data: Buffer): Buffer {
  const header = Buffer.alloc(8);
  header.writeUInt32BE(data.length, 0);
  header.write(type, 4, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([header.subarray(4), data])), 0);
  return Buffer.concat([header, data, crc]);
}

function paeth(left: number, up: number, upLeft: number): number {
  const estimate = left + up - upLeft;
  const toLeft = Math.abs(estimate - left);
  const toUp = Math.abs(estimate - up);
  const toUpLeft = Math.abs(estimate - upLeft);
  if (toLeft <= toUp && toLeft <= toUpLeft) return left;
  return toUp <= toUpLeft ? up : upLeft;
}

function unfilterRow(
  filter: number,
  source: Buffer,
  target: Buffer,
  previous: Buffer | undefined,
  bytesPerPixel: number,
): void {
  for (let index = 0; index < source.length; index += 1) {
    const left = index >= bytesPerPixel ? target[index - bytesPerPixel]! : 0;
    const up = previous ? previous[index]! : 0;
    const upLeft = previous && index >= bytesPerPixel ? previous[index - bytesPerPixel]! : 0;
    let predicted: number;
    switch (filter) {
      case 0:
        predicted = 0;
        break;
      case 1:
        predicted = left;
        break;
      case 2:
        predicted = up;
        break;
      case 3:
        predicted = (left + up) >> 1;
        break;
      case 4:
        predicted = paeth(left, up, upLeft);
        break;
      default:
        throw new Error(`unsupported PNG filter ${filter}`);
    }
    target[index] = (source[index]! + predicted) & 0xff;
  }
}

/** Decodes an 8-bit non-interlaced PNG into RGBA. */
export function decodePng(buffer: Buffer, label: string): RgbaImage {
  if (buffer.length < 8 || !buffer.subarray(0, 8).equals(PNG_SIGNATURE)) {
    throw new Error(`${label}: not a PNG`);
  }
  let offset = 8;
  let width = 0;
  let height = 0;
  let channels = 0;
  const data: Buffer[] = [];
  let sawHeader = false;
  while (offset + 12 <= buffer.length) {
    const length = buffer.readUInt32BE(offset);
    const type = buffer.toString("ascii", offset + 4, offset + 8);
    const body = buffer.subarray(offset + 8, offset + 8 + length);
    offset += length + 12;
    if (type === "IHDR") {
      width = body.readUInt32BE(0);
      height = body.readUInt32BE(4);
      const depth = body[8];
      const colorType = body[9];
      const interlace = body[12];
      if (depth !== 8 || interlace !== 0) {
        throw new Error(`${label}: ${depth}-bit interlaced=${interlace} PNG is not supported`);
      }
      channels = CHANNELS[colorType] ?? 0;
      if (channels === 0) throw new Error(`${label}: colour type ${colorType} is not supported`);
      sawHeader = true;
    } else if (type === "IDAT") {
      data.push(body);
    } else if (type === "IEND") {
      break;
    }
  }
  if (!sawHeader || width === 0 || height === 0) throw new Error(`${label}: missing IHDR`);

  const raw = inflateSync(Buffer.concat(data));
  const stride = width * channels;
  if (raw.length < (stride + 1) * height) throw new Error(`${label}: truncated pixel data`);
  const rows = Buffer.alloc(stride * height);
  for (let row = 0; row < height; row += 1) {
    const start = row * (stride + 1);
    const previous = row === 0 ? undefined : rows.subarray((row - 1) * stride, row * stride);
    unfilterRow(
      raw[start]!,
      raw.subarray(start + 1, start + 1 + stride),
      rows.subarray(row * stride, (row + 1) * stride),
      previous,
      channels,
    );
  }

  const pixels = Buffer.alloc(width * height * 4);
  for (let index = 0; index < width * height; index += 1) {
    const source = index * channels;
    const target = index * 4;
    if (channels === 1 || channels === 2) {
      const grey = rows[source]!;
      pixels[target] = grey;
      pixels[target + 1] = grey;
      pixels[target + 2] = grey;
    } else {
      pixels[target] = rows[source]!;
      pixels[target + 1] = rows[source + 1]!;
      pixels[target + 2] = rows[source + 2]!;
    }
    pixels[target + 3] = channels === 2 || channels === 4 ? rows[source + channels - 1]! : 0xff;
  }
  return { width, height, pixels };
}

/** Encodes RGBA pixels as a non-interlaced 8-bit PNG. */
export function encodePng(image: RgbaImage): Buffer {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(image.width, 0);
  header.writeUInt32BE(image.height, 4);
  header[8] = 8; // bit depth
  header[9] = 6; // colour type: RGBA

  const stride = image.width * 4;
  const raw = Buffer.alloc((stride + 1) * image.height);
  for (let row = 0; row < image.height; row += 1) {
    raw[row * (stride + 1)] = 0; // filter: none
    image.pixels.copy(raw, row * (stride + 1) + 1, row * stride, (row + 1) * stride);
  }

  return Buffer.concat([
    PNG_SIGNATURE,
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** Draws `panels` left to right on a white canvas of the tallest panel's height. */
export function composeSideBySide(panels: readonly RgbaImage[]): RgbaImage {
  if (panels.length === 0) throw new Error("no panels to compose");
  const height = Math.max(...panels.map((panel) => panel.height));
  const width =
    panels.reduce((total, panel) => total + panel.width, 0) + GUTTER_WIDTH * (panels.length - 1);
  const pixels = Buffer.alloc(width * height * 4);
  for (let index = 0; index < width * height; index += 1) {
    pixels.writeUInt32BE(0xffffffff, index * 4);
  }

  let x = 0;
  panels.forEach((panel, panelIndex) => {
    for (let row = 0; row < panel.height; row += 1) {
      panel.pixels.copy(
        pixels,
        (row * width + x) * 4,
        row * panel.width * 4,
        (row + 1) * panel.width * 4,
      );
    }
    x += panel.width;
    if (panelIndex < panels.length - 1) {
      for (let row = 0; row < height; row += 1) {
        for (let column = 0; column < GUTTER_WIDTH; column += 1) {
          const pixel = (row * width + x + column) * 4;
          GUTTER.forEach((value, channel) => {
            pixels[pixel + channel] = value;
          });
        }
      }
      x += GUTTER_WIDTH;
    }
  });

  return { width, height, pixels };
}

/** Every `-actual.png` under `outputDir` paired with the golden and diff written beside it. */
export function collectFailureArtifacts(outputDir: string): FailureArtifacts[] {
  const artifacts: FailureArtifacts[] = [];
  const walk = (directory: string): void => {
    let entries: Dirent[];
    try {
      entries = readdirSync(directory, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        walk(path);
        continue;
      }
      if (!entry.name.endsWith(ACTUAL_SUFFIX)) continue;
      const base = path.slice(0, -ACTUAL_SUFFIX.length);
      const expected = `${base}${EXPECTED_SUFFIX}`;
      const diff = `${base}${DIFF_SUFFIX}`;
      artifacts.push({
        label: relative(outputDir, path),
        actual: path,
        ...(existsSync(expected) ? { expected } : {}),
        ...(existsSync(diff) ? { diff } : {}),
      });
    }
  };
  walk(outputDir);
  return artifacts;
}

/** Writes `<outputDir>/side-by-side/<test dir>/<name>-side-by-side.png`; returns its path. */
export function writeSideBySide(artifact: FailureArtifacts, outputDir: string): string | undefined {
  const panels: RgbaImage[] = [];
  for (const path of [artifact.expected, artifact.actual, artifact.diff]) {
    if (!path) continue;
    panels.push(decodePng(readFileSync(path), path));
  }
  if (panels.length < 2) return undefined;

  const relativePath = relative(outputDir, artifact.actual);
  const target = join(
    outputDir,
    "side-by-side",
    dirname(relativePath),
    `${basename(relativePath, ACTUAL_SUFFIX)}-side-by-side.png`,
  );
  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, encodePng(composeSideBySide(panels)));
  return target;
}

/** Merges Playwright's expected/actual/diff artifacts into one image per failed comparison. */
export default class SideBySideReporter implements Reporter {
  private outputDir = "test-results";

  onBegin(config: FullConfig): void {
    // The config keeps Playwright's default output directory, resolved against the config file.
    this.outputDir = join(config.rootDir, "test-results");
  }

  onEnd(result: FullResult): void {
    if (result.status === "passed") return;
    let written = 0;
    try {
      for (const artifact of collectFailureArtifacts(this.outputDir)) {
        try {
          if (writeSideBySide(artifact, this.outputDir)) written += 1;
        } catch (error) {
          console.warn(`side-by-side: skipped ${artifact.label}: ${String(error)}`);
        }
      }
    } catch (error) {
      console.warn(`side-by-side: skipped the whole run: ${String(error)}`);
    }
    if (written > 0) {
      console.log(
        `side-by-side: wrote ${written} merged diff image(s) under ${join(this.outputDir, "side-by-side")}`,
      );
    }
  }
}
