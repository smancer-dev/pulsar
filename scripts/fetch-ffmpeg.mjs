#!/usr/bin/env node
// fetch-ffmpeg.mjs — download a STATIC ffmpeg binary for the current
// (or a forced) platform into desktop-app/src-tauri/resources/.
//
// Pulsar bundles ffmpeg so end users install nothing. We are GPLv3, so the
// GPL ffmpeg builds below are license-compatible. Each chosen build includes
// the screen-capture indev + libx264 we need:
//   - Windows x64 : gdigrab     + libx264   (BtbN GPL)
//   - Linux  x64  : x11grab     + libx264   (BtbN GPL)
//   - Linux  arm64: x11grab     + libx264   (BtbN GPL)
//   - macOS  x64  : avfoundation+ libx264   (evermeet.cx)
//   - macOS  arm64: avfoundation+ libx264   (Martin Riedl)
//
// Output name: resources/ffmpeg  (or ffmpeg.exe on Windows).
//
// Usage:
//   node scripts/fetch-ffmpeg.mjs                 # auto-detect host
//   node scripts/fetch-ffmpeg.mjs --os linux --arch arm64   # force target
//   FFMPEG_TARGET_OS=macos FFMPEG_TARGET_ARCH=arm64 node scripts/fetch-ffmpeg.mjs
//
// In CI the host runner already matches the wanted OS for native builds, so a
// plain `node scripts/fetch-ffmpeg.mjs` Just Works on each matrix runner.
// (For Linux arm64 you typically build on an x64 runner and cross-fetch with
//  --os linux --arch arm64.)

import { createWriteStream } from "node:fs";
import { mkdir, rm, chmod, readdir, stat, rename, mkdtemp } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { pipeline } from "node:stream/promises";
import { Readable } from "node:stream";

const __dirname = fileURLToPath(new URL(".", import.meta.url));
const RESOURCES = join(__dirname, "..", "src-tauri", "resources");

// --- target resolution -------------------------------------------------------
function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i !== -1 ? process.argv[i + 1] : undefined;
}

function detectOs() {
  const o = (arg("os") || process.env.FFMPEG_TARGET_OS || "").toLowerCase();
  if (o) return o;
  // GitHub Actions sets RUNNER_OS = Windows | macOS | Linux
  const r = (process.env.RUNNER_OS || "").toLowerCase();
  if (r) return r === "macos" ? "macos" : r === "windows" ? "windows" : "linux";
  return process.platform === "win32"
    ? "windows"
    : process.platform === "darwin"
      ? "macos"
      : "linux";
}

function detectArch() {
  const a = (arg("arch") || process.env.FFMPEG_TARGET_ARCH || "").toLowerCase();
  if (a) return a === "aarch64" ? "arm64" : a;
  // GitHub Actions sets RUNNER_ARCH = X64 | ARM64 | X86
  const r = (process.env.RUNNER_ARCH || "").toLowerCase();
  if (r) return r === "arm64" ? "arm64" : "x64";
  return process.arch === "arm64" ? "arm64" : "x64";
}

const OS = detectOs();
const ARCH = detectArch();

// --- per-target source -------------------------------------------------------
// BtbN tag "latest" + version-agnostic "master-latest-*" filenames are stable,
// so these URLs do not need bumping when ffmpeg releases. macOS uses redirect /
// getrelease endpoints that always resolve to the newest build.
const BTBN = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest";

const SOURCES = {
  "windows/x64": {
    url: `${BTBN}/ffmpeg-master-latest-win64-gpl.zip`,
    kind: "zip",
    // archive layout: ffmpeg-*-win64-gpl/bin/ffmpeg.exe
    member: /\/bin\/ffmpeg\.exe$/i,
  },
  "linux/x64": {
    url: `${BTBN}/ffmpeg-master-latest-linux64-gpl.tar.xz`,
    kind: "tar.xz",
    member: /\/bin\/ffmpeg$/,
  },
  "linux/arm64": {
    url: `${BTBN}/ffmpeg-master-latest-linuxarm64-gpl.tar.xz`,
    kind: "tar.xz",
    member: /\/bin\/ffmpeg$/,
  },
  // evermeet.cx: stable "getrelease" endpoint, always-latest, Intel x86_64,
  // single ffmpeg binary inside the zip (avfoundation + libx264).
  "macos/x64": {
    url: "https://evermeet.cx/ffmpeg/getrelease/zip",
    kind: "zip",
    member: /(^|\/)ffmpeg$/,
  },
  // Martin Riedl build server: stable redirect URL for the latest macOS arm64
  // release build (avfoundation + libx264, signed/notarized).
  "macos/arm64": {
    url: "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffmpeg.zip",
    kind: "zip",
    member: /(^|\/)ffmpeg$/,
  },
};

const key = `${OS}/${ARCH}`;
const src = SOURCES[key];
if (!src) {
  console.error(`No ffmpeg source mapped for target "${key}".`);
  console.error(`Supported: ${Object.keys(SOURCES).join(", ")}`);
  process.exit(1);
}

const OUT_NAME = OS === "windows" ? "ffmpeg.exe" : "ffmpeg";
const OUT_PATH = join(RESOURCES, OUT_NAME);

// --- helpers -----------------------------------------------------------------
// Some shells leak env vars containing NUL bytes, which node's spawn rejects.
// Build a clean copy that drops any such values.
function cleanEnv() {
  const out = {};
  for (const [k, v] of Object.entries(process.env)) {
    if (typeof v === "string" && !v.includes("\x00")) out[k] = v;
  }
  return out;
}

function run(cmd, args, opts = {}) {
  return new Promise((resolve, reject) => {
    const p = spawn(cmd, args, { stdio: "inherit", env: cleanEnv(), ...opts });
    p.on("error", reject);
    p.on("close", (code) =>
      code === 0 ? resolve() : reject(new Error(`${cmd} exited ${code}`)),
    );
  });
}

async function download(url, dest, redirectsLeft = 6) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok || !res.body) {
    throw new Error(`download failed ${res.status} ${res.statusText} for ${url}`);
  }
  await pipeline(Readable.fromWeb(res.body), createWriteStream(dest));
}

async function findFile(dir, re) {
  const entries = await readdir(dir, { withFileTypes: true });
  for (const e of entries) {
    const full = join(dir, e.name);
    if (e.isDirectory()) {
      const hit = await findFile(full, re);
      if (hit) return hit;
    } else if (re.test(full.replace(/\\/g, "/"))) {
      return full;
    }
  }
  return null;
}

// --- main --------------------------------------------------------------------
async function main() {
  console.log(`Fetching ffmpeg for ${key}`);
  console.log(`  source: ${src.url}`);

  await mkdir(RESOURCES, { recursive: true });
  const work = await mkdtemp(join(tmpdir(), "pulsar-ffmpeg-"));
  const archive = join(work, src.kind === "zip" ? "ffmpeg.zip" : "ffmpeg.tar.xz");

  try {
    await download(src.url, archive);

    const extractDir = join(work, "out");
    await mkdir(extractDir, { recursive: true });

    if (src.kind === "tar.xz") {
      // `tar` on every GitHub runner (incl. Windows bsdtar) handles .tar.xz.
      await run("tar", ["-xf", archive, "-C", extractDir]);
    } else {
      // zip: prefer `unzip`, fall back to PowerShell Expand-Archive on Windows,
      // then to `tar` (bsdtar reads zips too).
      if (await hasCmd("unzip")) {
        await run("unzip", ["-q", "-o", archive, "-d", extractDir]);
      } else if (process.platform === "win32") {
        await run("powershell", [
          "-NoProfile",
          "-Command",
          `Expand-Archive -LiteralPath '${archive}' -DestinationPath '${extractDir}' -Force`,
        ]);
      } else {
        await run("tar", ["-xf", archive, "-C", extractDir]);
      }
    }

    const found = await findFile(extractDir, src.member);
    if (!found) {
      throw new Error(
        `could not locate ffmpeg binary (pattern ${src.member}) in extracted archive`,
      );
    }

    if (existsSync(OUT_PATH)) await rm(OUT_PATH, { force: true });
    await rename(found, OUT_PATH).catch(async () => {
      // rename across devices (tmp -> repo) can EXDEV; copy instead.
      const { copyFile } = await import("node:fs/promises");
      await copyFile(found, OUT_PATH);
    });
    if (OS !== "windows") await chmod(OUT_PATH, 0o755);

    const s = await stat(OUT_PATH);
    console.log(`Wrote ${OUT_PATH} (${(s.size / 1e6).toFixed(1)} MB)`);
  } finally {
    await rm(work, { recursive: true, force: true });
  }
}

async function hasCmd(cmd) {
  try {
    await run(process.platform === "win32" ? "where" : "which", [cmd], {
      stdio: "ignore",
    });
    return true;
  } catch {
    return false;
  }
}

main().catch((err) => {
  console.error(err.message || err);
  process.exit(1);
});
