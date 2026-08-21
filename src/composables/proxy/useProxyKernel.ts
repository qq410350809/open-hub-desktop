import { ref } from "vue";
import { listen } from "@tauri-apps/api/event";
import { runCommand, isTauri } from "../core/ipc";
import type {
  GeoipDownloadProgress,
  GeoipStatus,
  MihomoDownloadProgress,
  MihomoKernelStatus,
} from "../../types";

export const KERNEL_DOWNLOAD_MIRRORS = [
  { value: "auto", text: "⚡ 智能全网竞速 (推荐 · 4线程并发)" },
  { value: "https://gh-proxy.com", text: "🚀 gh-proxy.com (亚太 CDN)" },
  { value: "https://ghfast.top", text: "🚀 ghfast.top (Cloudflare 边缘加速)" },
  { value: "https://gh.ddlc.top", text: "🚀 gh.ddlc.top (国内边缘加速)" },
  { value: "https://ghps.cc", text: "🚀 ghps.cc (国内镜像)" },
  { value: "https://github.boki.moe", text: "🚀 github.boki.moe (镜像加速)" },
  { value: "https://ghproxy.net", text: "🚀 ghproxy.net (备用镜像)" },
  { value: "direct", text: "🌐 GitHub 官方直连 (适合 VPN/代理)" },
  { value: "custom", text: "⚙️ 自定义镜像源前缀" },
] as const;

export const kernelSelectedMirror = ref<string>("auto");
export const kernelCustomMirror = ref<string>("");

export const kernelStatus = ref<MihomoKernelStatus | null>(null);
export const kernelLoading = ref(false);
export const kernelChecking = ref(false);
export const kernelDownloading = ref(false);
export const kernelDownloadProgress = ref<MihomoDownloadProgress>({
  stage: "",
  progress: 0,
  message: "",
});

export const geoipStatus = ref<GeoipStatus | null>(null);
export const geoipLoading = ref(false);
export const geoipDownloading = ref(false);
export const geoipDownloadProgress = ref<GeoipDownloadProgress>({
  stage: "",
  progress: 0,
  message: "",
});

if (isTauri) {
  listen<MihomoDownloadProgress>("mihomo-kernel-progress", (event) => {
    kernelDownloadProgress.value = event.payload;
  });
  listen<GeoipDownloadProgress>("geoip-download-progress", (event) => {
    geoipDownloadProgress.value = event.payload;
  });
}

export async function loadMihomoKernelStatus() {
  kernelLoading.value = true;
  try {
    kernelStatus.value = await runCommand<MihomoKernelStatus>("get_mihomo_kernel_status");
  } catch (err) {
    console.error("读取 Mihomo 内核状态失败", err);
  } finally {
    kernelLoading.value = false;
  }
}

export async function checkMihomoKernelUpdate(mirror?: string) {
  const m = mirror ?? (kernelSelectedMirror.value === "custom" ? kernelCustomMirror.value : kernelSelectedMirror.value);
  kernelChecking.value = true;
  try {
    kernelStatus.value = await runCommand<MihomoKernelStatus>("check_mihomo_kernel_update", { mirror: m || null });
    return kernelStatus.value;
  } finally {
    kernelChecking.value = false;
  }
}

export async function downloadOrUpdateMihomoKernel(mirror?: string, onCompleted?: () => Promise<void>) {
  const m = mirror ?? (kernelSelectedMirror.value === "custom" ? kernelCustomMirror.value : kernelSelectedMirror.value);
  kernelDownloading.value = true;
  kernelDownloadProgress.value = { stage: "starting", progress: 0, message: "准备下载…" };
  try {
    kernelStatus.value = await runCommand<MihomoKernelStatus>("download_or_update_mihomo_kernel", { mirror: m || null });
    if (onCompleted) await onCompleted();
    return kernelStatus.value;
  } finally {
    kernelDownloading.value = false;
  }
}

export async function loadGeoipStatus() {
  geoipLoading.value = true;
  try {
    geoipStatus.value = await runCommand<GeoipStatus>("get_geoip_status");
  } catch (err) {
    console.error("读取 GeoIP 状态失败", err);
  } finally {
    geoipLoading.value = false;
  }
}

export async function downloadOrUpdateGeoip(mirror?: string, onCompleted?: () => Promise<void>) {
  const m = mirror ?? (kernelSelectedMirror.value === "custom" ? kernelCustomMirror.value : kernelSelectedMirror.value);
  geoipDownloading.value = true;
  geoipDownloadProgress.value = { stage: "starting", progress: 0, message: "准备下载 GeoIP 数据库…" };
  try {
    geoipStatus.value = await runCommand<GeoipStatus>("download_or_update_geoip", { mirror: m || null });
    if (onCompleted) await onCompleted();
    return geoipStatus.value;
  } finally {
    geoipDownloading.value = false;
  }
}
