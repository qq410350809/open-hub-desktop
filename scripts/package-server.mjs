import { cp, mkdir, rm, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";

const root = resolve(import.meta.dirname, "..");
const outputRoot = resolve(root, process.env.SERVER_PACKAGE_DIR || "dist-server");
const platform = process.env.SERVER_PLATFORM || process.platform;
const architecture = process.env.SERVER_ARCH || process.arch;
const binaryName = platform === "win32" ? "openhub-server.exe" : "openhub-server";
const sourceBinary = resolve(root, "src-tauri/target/release", binaryName);
const sourceDist = resolve(root, "dist");
const packageName = `openhub-server-${platform}-${architecture}`;
const packageDir = resolve(outputRoot, packageName);

await rm(packageDir, { recursive: true, force: true });
await mkdir(packageDir, { recursive: true });
await cp(sourceBinary, resolve(packageDir, binaryName));
await cp(sourceDist, resolve(packageDir, "dist"), { recursive: true });
await writeFile(
  resolve(packageDir, "README.txt"),
  [
    "OpenHub standalone server",
    "",
    `Platform: ${platform}/${architecture}`,
    "",
    "Run the binary from this directory so it can find the sibling dist/ folder:",
    platform === "win32"
      ? `  .\\${binaryName} --listen 17896`
      : `  ./${binaryName} --listen 17896`,
    "",
    "Open the Web UI and sign in. Sessions are valid for 7 days. All remote requests require a valid login session.",
    "",
    "Model APIs share the same port: /v1/* and /v1beta/* require the gateway API key",
    "via Authorization: Bearer / x-api-key headers (never URL query params).",
    "A key is auto-generated on first start and stored in the SQLite app_meta config;",
    "view or copy it from Web UI -> 模型网关 page.",
  ].join("\n"),
);

const files = [binaryName, "README.txt"].map((file) => resolve(packageDir, file));
for (const file of files) {
  const hash = createHash("sha256").update(await readFile(file)).digest("hex");
  await writeFile(`${file}.sha256`, `${hash}  ${basename(file)}\n`);
}
console.log(`Prepared ${packageDir}`);
