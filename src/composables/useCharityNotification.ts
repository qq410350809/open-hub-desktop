import { onMounted, onUnmounted } from "vue";

interface CharityNewMessageEvent {
  feedName: string;
  newCount: number;
  updatedCount: number;
  timestamp: number;
}

let audioContext: AudioContext | null = null;
let notificationEnabled = true;

// 在指定时间点安排一个音符（快速起音 + 指数衰减，模拟清脆铃声）
function scheduleTone(ctx: AudioContext, freq: number, start: number, duration: number, peakGain: number) {
  const oscillator = ctx.createOscillator();
  const gainNode = ctx.createGain();

  oscillator.connect(gainNode);
  gainNode.connect(ctx.destination);

  oscillator.type = "sine";
  oscillator.frequency.value = freq;

  // 快速起音后指数衰减，收尾拉长让余音更饱满
  gainNode.gain.setValueAtTime(0.0001, start);
  gainNode.gain.exponentialRampToValueAtTime(peakGain, start + 0.02);
  gainNode.gain.exponentialRampToValueAtTime(0.0001, start + duration);

  oscillator.start(start);
  oscillator.stop(start + duration + 0.05);
}

// 连响轮数与轮间间隔
const CHIME_REPEAT = 3;
const CHIME_ROUND_GAP = 1.0; // 秒

// 生成连响的“叮咚”提示音（使用 Web Audio API）
function playNotificationSound() {
  try {
    if (!audioContext) {
      audioContext = new (window.AudioContext || (window as any).webkitAudioContext)();
    }

    // 后台事件触发时音频上下文可能被系统挂起，先尝试恢复
    if (audioContext.state === "suspended") {
      void audioContext.resume();
    }

    const now = audioContext.currentTime + 0.02;
    // 每轮“叮→咚”清脆两声，间隔 1 秒连响三轮，整体约 3 秒
    for (let round = 0; round < CHIME_REPEAT; round++) {
      const base = now + round * CHIME_ROUND_GAP;
      scheduleTone(audioContext, 988, base, 0.35, 0.55);       // B5 叮
      scheduleTone(audioContext, 784, base + 0.28, 0.75, 0.6); // G5 咚
    }
  } catch (error) {
    console.warn("播放提示音失败:", error);
  }
}

// 发送系统通知（Tauri 桌面端走原生通知插件，浏览器环境回退 Web Notification API）
async function sendSystemNotification(event: CharityNewMessageEvent) {
  const title = "公益监听 - 新消息提醒";
  const body = `「${event.feedName}」有新动态：新增 ${event.newCount} 条，更新 ${event.updatedCount} 条`;

  if (typeof window !== "undefined" && (window as any).__TAURI__) {
    // WKWebView / WebView2 不支持 Web Notification，必须走原生插件
    const { isPermissionGranted, requestPermission, sendNotification } = await import(
      "@tauri-apps/plugin-notification"
    );

    let granted = await isPermissionGranted();
    if (!granted) {
      granted = (await requestPermission()) === "granted";
    }
    if (!granted) {
      console.warn("系统通知权限未授权，无法弹出通知");
      return;
    }

    sendNotification({ title, body });
    return;
  }

  if (!("Notification" in window)) {
    console.warn("当前浏览器不支持系统通知");
    return;
  }

  // 请求通知权限
  if (Notification.permission === "default") {
    const permission = await Notification.requestPermission();
    if (permission !== "granted") {
      return;
    }
  }

  if (Notification.permission === "granted") {
    const notification = new Notification(title, {
      body,
      icon: "/icon.png",
      tag: "charity-notification", // 同一标签会替换旧通知
      requireInteraction: false,
      silent: false,
    });

    // 点击通知时聚焦窗口
    notification.onclick = () => {
      window.focus();
      notification.close();
    };

    // 5秒后自动关闭
    setTimeout(() => {
      notification.close();
    }, 5000);
  }
}

// 发送一条测试通知（提示音 + 系统通知），走与真实新消息相同的展示逻辑。
// 独立导出：页面里的"测试"按钮无需注册事件监听即可调用。
export function sendCharityTestNotification() {
  playNotificationSound();

  void sendSystemNotification({
    feedName: "测试订阅源",
    newCount: 1,
    updatedCount: 0,
    timestamp: Math.floor(Date.now() / 1000),
  });
}

// 处理新消息事件
function handleNewMessage(event: CharityNewMessageEvent) {
  if (!notificationEnabled) {
    return;
  }

  console.log("公益监听收到新消息:", event);

  // 播放提示音
  playNotificationSound();

  // 发送系统通知
  void sendSystemNotification(event);
}

export function useCharityNotification() {
  let unlisten: (() => void) | null = null;

  onMounted(async () => {
    // 检查是否在 Tauri 环境
    if (typeof window !== "undefined" && (window as any).__TAURI__) {
      const { listen } = await import("@tauri-apps/api/event");

      unlisten = await listen<CharityNewMessageEvent>("charity-new-message", (event) => {
        handleNewMessage(event.payload);
      });

      console.log("公益监听通知已启用");
    }

    // 预先申请通知权限，避免首条真实通知被权限弹窗吞掉
    if (typeof window !== "undefined" && (window as any).__TAURI__) {
      const { requestPermission } = await import("@tauri-apps/plugin-notification");
      void requestPermission();
    } else if ("Notification" in window && Notification.permission === "default") {
      void Notification.requestPermission();
    }
  });

  onUnmounted(() => {
    if (unlisten) {
      unlisten();
    }
  });

  // 返回控制方法
  return {
    enable: () => {
      notificationEnabled = true;
    },
    disable: () => {
      notificationEnabled = false;
    },
    isEnabled: () => notificationEnabled,
    sendTestNotification: sendCharityTestNotification,
  };
}
