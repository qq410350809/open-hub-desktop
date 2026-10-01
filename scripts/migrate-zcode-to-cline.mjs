#!/usr/bin/env node
/**
 * 把 ZCode 的供应商 / 模型清单迁移到 Cline。
 *
 * 源：`~/.zcode/v2/config.json` 的 `provider.*`
 *     （name / kind / options.baseURL / options.apiKey / models.*）
 * 目标：Cline 自定义供应商
 *     - `~/.cline/data/settings/providers.json`（每家一条 settings）
 *     - `~/.cline/data/settings/models.json`（自定义供应商注册表 + 模型清单）
 *     - `~/.cline/data/settings/.zcode-migration-map.json`（ZCode id ↔ Cline id，
 *       保证重复执行是「更新」而不是「再建一批」）
 *
 * 说明：
 * - 只处理 openai / openai-compatible 协议的供应商；Cline 的自定义供应商注册表
 *   只支持 openai-chat，anthropic 协议的条目（Z.ai、BigModel、agentrouter 等）
 *   会被跳过并列出。
 * - Cline 的供应商 id 是扁平命名空间，ZCode 里的 UUID 换成人名（name 派生 slug），
 *   与 Cline 内置 id 撞名时加 `-zcode` 后缀。
 * - 幂等：已迁移过的条目走 update；用户已在 Cline 里删掉的条目不再重建。
 *   不动 Cline 自带的 cline / anthropic / sapaicore 等条目，也不改 lastUsedProvider。
 *
 * 用法：
 *   node scripts/migrate-zcode-to-cline.mjs --dry-run   # 只看会写什么
 *   node scripts/migrate-zcode-to-cline.mjs             # 实际写入
 */

import { copyFileSync, existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const ZCODE_CONFIG = join(homedir(), ".zcode", "v2", "config.json");
const CLINE_SETTINGS_DIR = join(homedir(), ".cline", "data", "settings");
const CLINE_PROVIDERS = join(CLINE_SETTINGS_DIR, "providers.json");
const CLINE_MODELS = join(CLINE_SETTINGS_DIR, "models.json");
const CLINE_MAP = join(CLINE_SETTINGS_DIR, ".zcode-migration-map.json");

const CORE_HINT =
	"未找到 @clinebot/core。请确认 Cline CLI 已安装（如 `npm i -g kanban`），" +
	"或用 CLINE_CORE_PATH 指定 core 的 dist/index.js 路径。";

/** Cline CLI 由其它 npm 包内联安装，逐个候选根目录找。 */
async function loadCore() {
	const candidates = [];
	if (process.env.CLINE_CORE_PATH) candidates.push(process.env.CLINE_CORE_PATH);

	const roots = [
		"/usr/local/lib/node_modules",
		"/opt/homebrew/lib/node_modules",
		join(homedir(), ".npm-global", "lib", "node_modules"),
		"/usr/lib/node_modules",
	];
	for (const root of roots) {
		candidates.push(join(root, "@clinebot", "core", "dist", "index.js"));
		let entries = [];
		try {
			entries = readdirSync(root);
		} catch {
			continue;
		}
		for (const entry of entries) {
			candidates.push(
				join(root, entry, "node_modules", "@clinebot", "core", "dist", "index.js"),
			);
		}
	}

	for (const candidate of candidates) {
		if (!existsSync(candidate)) continue;
		const mod = await import(pathToFileURL(candidate).href);
		if (mod.ProviderSettingsManager && mod.addLocalProvider) return mod;
	}
	throw new Error(CORE_HINT);
}

/** ZCode 供应商名 -> Cline 供应商 id（^[a-z0-9][a-z0-9-]*$）。 */
function slugify(name) {
	const slug = String(name ?? "")
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, "-")
		.replace(/^-+|-+$/g, "");
	return /^[a-z0-9]/.test(slug) ? slug : "";
}

function uniqueId(base, taken) {
	if (!taken.has(base)) return base;
	const withSuffix = `${base}-zcode`;
	if (!taken.has(withSuffix)) return withSuffix;
	for (let i = 2; ; i += 1) {
		const candidate = `${withSuffix}-${i}`;
		if (!taken.has(candidate)) return candidate;
	}
}

function readJson(path) {
	if (!existsSync(path)) return undefined;
	try {
		return JSON.parse(readFileSync(path, "utf8"));
	} catch {
		return undefined;
	}
}

const OPENAI_KINDS = new Set(["openai", "openai-compatible"]);

/**
 * 由单家 ZCode 供应商算出迁移动作。
 * - 映射表里有记录且 Cline 侧还在 -> update
 * - 映射表里有记录但 Cline 侧已被删掉 -> skip（尊重用户的手动删除）
 * - 没有记录 -> create，id 从 name 派生并避开已占用的
 */
function planProvider(zcodeId, entry, ctx) {
	const name = typeof entry.name === "string" ? entry.name.trim() : "";
	const options = entry.options ?? {};
	const baseUrl = typeof options.baseURL === "string" ? options.baseURL.trim() : "";
	const apiKey = typeof options.apiKey === "string" ? options.apiKey.trim() : "";
	const modelsObj = entry.models && typeof entry.models === "object" ? entry.models : {};

	const modelIds = Object.keys(modelsObj).filter((id) => id.trim().length > 0);
	if (!baseUrl || modelIds.length === 0) return undefined;

	const flags = modelIds.map((id) => {
		const model = modelsObj[id] ?? {};
		const inputs = model.modalities?.input ?? [];
		return {
			id,
			name: typeof model.name === "string" ? model.name : id,
			reasoning: model.reasoning?.enabled === true,
			vision: inputs.includes("image"),
		};
	});

	const capabilities = ["streaming", "tools"];
	if (flags.some((m) => m.reasoning)) capabilities.push("reasoning");
	if (flags.some((m) => m.vision)) capabilities.push("vision");

	const mapped = ctx.map[zcodeId]?.clineProviderId;
	let providerId;
	let action;
	if (mapped) {
		if (ctx.clineIds.has(mapped)) {
			providerId = mapped;
			action = "update";
		} else {
			providerId = mapped;
			action = "skip-deleted";
		}
	} else {
		const base = slugify(name) || slugify(zcodeId) || "zcode-provider";
		providerId = uniqueId(base, ctx.takenIds);
		ctx.takenIds.add(providerId);
		action = "create";
	}

	return {
		zcodeId,
		providerId,
		action,
		name: name || providerId,
		baseUrl,
		apiKey,
		modelIds,
		defaultModelId: modelIds[0],
		capabilities,
	};
}

function stamp() {
	const d = new Date();
	const p = (n, w = 2) => String(n).padStart(w, "0");
	return (
		`${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}` +
		`-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`
	);
}

async function main() {
	const dryRun = process.argv.includes("--dry-run");

	if (!existsSync(ZCODE_CONFIG)) {
		throw new Error(`未找到 ZCode 配置：${ZCODE_CONFIG}`);
	}
	const zcode = JSON.parse(readFileSync(ZCODE_CONFIG, "utf8"));
	const providers = zcode.provider ?? {};

	const core = await loadCore();
	const {
		ProviderSettingsManager,
		addLocalProvider,
		updateLocalProvider,
		BUILT_IN_PROVIDER_IDS,
	} = core;

	const existingModels = readJson(CLINE_MODELS);
	const clineIds = new Set(Object.keys(existingModels?.providers ?? {}));
	const existingProviders = readJson(CLINE_PROVIDERS);
	const takenIds = new Set(BUILT_IN_PROVIDER_IDS);
	for (const id of clineIds) takenIds.add(id);
	for (const id of Object.keys(existingProviders?.providers ?? {})) takenIds.add(id);

	const mapState = readJson(CLINE_MAP) ?? {};
	mapState.version = 1;
	if (!mapState.providers || typeof mapState.providers !== "object") {
		mapState.providers = {};
	}
	const ctx = { map: mapState.providers, takenIds, clineIds };

	const plans = [];
	const skipped = [];
	for (const [zcodeId, entry] of Object.entries(providers)) {
		if (!OPENAI_KINDS.has(entry?.kind)) {
			skipped.push({
				reason: `协议 ${entry?.kind ?? "?"} 不是 openai-chat`,
				name: entry?.name ?? zcodeId,
				kind: entry?.kind ?? "?",
			});
			continue;
		}
		const plan = planProvider(zcodeId, entry, ctx);
		if (plan) plans.push(plan);
		else {
			skipped.push({
				reason: "缺少 baseURL 或模型清单为空",
				name: entry?.name ?? zcodeId,
				kind: entry?.kind,
			});
		}
	}

	const creates = plans.filter((p) => p.action === "create");
	const updates = plans.filter((p) => p.action === "update");
	const deleted = plans.filter((p) => p.action === "skip-deleted");

	console.log(
		`ZCode 供应商 ${Object.keys(providers).length} 家 → 新增 ${creates.length}、` +
			`更新 ${updates.length}、Cline 侧已删除 ${deleted.length}`,
	);
	console.log();
	for (const plan of plans) {
		const tag =
			plan.action === "create" ? "新增" : plan.action === "update" ? "更新" : "已删除";
		if (plan.action === "skip-deleted") {
			console.log(`  [${tag}] ${plan.providerId}（${plan.name}）`);
			continue;
		}
		const note = plan.apiKey ? "" : "  [无 apiKey]";
		console.log(
			`  [${tag}] ${plan.providerId.padEnd(24)} ${String(plan.modelIds.length).padStart(3)} 个模型  ` +
				`${plan.capabilities.join("/")}${note}`,
		);
		console.log(`      ${plan.baseUrl}`);
	}
	if (skipped.length > 0) {
		console.log();
		console.log(`跳过 ${skipped.length} 家：`);
		for (const item of skipped) console.log(`  ${item.name}（${item.kind}）：${item.reason}`);
	}

	if (dryRun) {
		console.log();
		console.log("--dry-run：未写入任何文件。");
		return;
	}

	const writable = [...creates, ...updates];
	if (writable.length === 0) {
		console.log();
		console.log("没有需要写入的供应商。");
		return;
	}

	// 备份：Cline 自己不做备份，写前留一份。
	const tag = stamp();
	const backedUp = [];
	for (const path of [CLINE_PROVIDERS, CLINE_MODELS]) {
		if (!existsSync(path)) continue;
		const target = `${path}.bak-zcode-${tag}`;
		copyFileSync(path, target);
		backedUp.push(target);
	}

	const manager = new ProviderSettingsManager({ filePath: CLINE_PROVIDERS });
	let written = 0;
	for (const plan of writable) {
		const request = {
			providerId: plan.providerId,
			name: plan.name,
			baseUrl: plan.baseUrl,
			apiKey: plan.apiKey || undefined,
			models: plan.modelIds,
			defaultModelId: plan.defaultModelId,
			capabilities: plan.capabilities,
		};
		try {
			if (plan.action === "create") await addLocalProvider(manager, request);
			else await updateLocalProvider(manager, request);
			mapState.providers[plan.zcodeId] = {
				clineProviderId: plan.providerId,
				name: plan.name,
				baseUrl: plan.baseUrl,
			};
			written += 1;
		} catch (error) {
			console.error(`  写入失败 ${plan.providerId}: ${error.message}`);
		}
	}
	mapState.version = 1;
	writeFileSync(CLINE_MAP, `${JSON.stringify(mapState, null, 2)}\n`, "utf8");

	console.log();
	console.log(`已写入 ${written}/${writable.length} 家到：`);
	console.log(`  ${CLINE_PROVIDERS}`);
	console.log(`  ${CLINE_MODELS}`);
	console.log(`  ${CLINE_MAP}`);
	if (backedUp.length > 0) {
		console.log("备份：");
		for (const path of backedUp) console.log(`  ${path}`);
	}
	console.log();
	console.log(`跳过 ${skipped.length} 家（见上），生效需重启 Cline。`);
}

main().catch((error) => {
	console.error(error.message);
	process.exit(1);
});
