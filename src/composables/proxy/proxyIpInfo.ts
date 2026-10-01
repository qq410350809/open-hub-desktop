import type { ProxyIpInfo, ProxyNode } from "../../types";

export const PROXY_IP_KIND_LABELS: Record<ProxyIpInfo["kind"], string> = {
  hosting: "云服务器/机房",
  residential: "疑似家宽",
  mobile: "移动网络",
  unknown: "未知",
};

// 与后端 ip_info.rs 一致：成功缓存 24h，失败 5min；缓存可早于本次 testedAt。
export function validProxyIpInfo(node: Pick<ProxyNode, "primaryIp" | "ipInfo">): ProxyIpInfo | null {
  const info = node.ipInfo;
  if (!info || !node.primaryIp || info.ip !== node.primaryIp) return null;
  const checkedAt = Date.parse(info.checkedAt);
  const age = Date.now() - checkedAt;
  const ttl = info.status === "success" ? 24 * 60 * 60_000 : 5 * 60_000;
  if (!Number.isFinite(checkedAt) || age < 0 || age >= ttl) return null;
  return info;
}

export function proxyIpKind(node: Pick<ProxyNode, "primaryIp" | "ipInfo">): ProxyIpInfo["kind"] | "unidentified" {
  const info = validProxyIpInfo(node);
  if (!info) return "unidentified";
  return info.status === "success" && Object.hasOwn(PROXY_IP_KIND_LABELS, info.kind) ? info.kind : "unknown";
}

export function proxyIpLabel(node: Pick<ProxyNode, "primaryIp" | "ipInfo">): string {
  const kind = proxyIpKind(node);
  return kind === "unidentified" ? "未识别" : PROXY_IP_KIND_LABELS[kind];
}

export function proxyIpSearchText(node: ProxyNode): string {
  const info = validProxyIpInfo(node);
  return [proxyIpLabel(node), proxyIpKind(node), info?.isp, info?.organization, info?.asn].filter(Boolean).join(" ");
}

export function proxyIpDetails(node: ProxyNode): string {
  const info = validProxyIpInfo(node);
  return [
    `出口 IP：${node.primaryIp || "未获取"}`,
    `IP 类型：${proxyIpLabel(node)}`,
    ...(info ? [
      `ISP：${info.isp || "未知"}`,
      `组织：${info.organization || "未知"}`,
      `ASN：${info.asn || "未知"}`,
      `查询来源：${info.source || "未知"}`,
      `查询时间：${new Date(info.checkedAt).toLocaleString()}`,
      info.status === "error" ? `查询失败：${info.error || "未提供失败原因"}（不影响测速）` : "",
    ] : ["尚无与当前出口匹配的有效查询结果；测速成功后自动识别。"]),
    "结果仅供参考，可能来自缓存；疑似家宽不保证为住宅网络。",
  ].filter(Boolean).join("\n");
}
