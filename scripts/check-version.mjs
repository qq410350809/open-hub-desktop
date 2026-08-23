import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const packageJson = JSON.parse(await readFile(resolve(root, "package.json"), "utf8"));
const cargoToml = await readFile(resolve(root, "src-tauri/Cargo.toml"), "utf8");
const tauriConfig = JSON.parse(await readFile(resolve(root, "src-tauri/tauri.conf.json"), "utf8"));

const cargoVersion = cargoToml.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const tauriVersion = tauriConfig.version;
const versions = {
  "package.json": packageJson.version,
  "src-tauri/Cargo.toml": cargoVersion,
  "src-tauri/tauri.conf.json": tauriVersion,
};

const uniqueVersions = new Set(Object.values(versions));
if (uniqueVersions.size !== 1 || [...uniqueVersions][0] == null) {
  console.error("版本号不一致：");
  for (const [file, version] of Object.entries(versions)) {
    console.error(`  ${file}: ${version ?? "<missing>"}`);
  }
  process.exit(1);
}

const expectedTag = process.env.RELEASE_VERSION;
if (expectedTag && expectedTag.replace(/^v/, "") !== packageJson.version) {
  console.error(
    `发布标签 ${expectedTag} 与项目版本 ${packageJson.version} 不一致`,
  );
  process.exit(1);
}

console.log(`OpenHub version ${packageJson.version} is consistent`);
