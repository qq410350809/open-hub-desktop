import { onMounted, onUnmounted } from "vue";
import { isTauri, runCommand } from "./core/ipc";
import { useToast } from "./core/useToast";

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

// 发送系统通知（Tauri 桌面端走自研 UNUserNotificationCenter 命令，浏览器环境
// 回退 Web Notification API）。返回 null 表示已发送，否则返回失败原因。
// 说明：tauri-plugin-notification 在 macOS 落到已废弃的 NSUserNotification API，
// 应用前台不弹横幅、dev 模式下通知被系统静默丢弃（历史记录为空），故弃用。
async function sendSystemNotification(event: CharityNewMessageEvent): Promise<string | null> {
  const title = "公益监听 - 新消息提醒";
  const body = `「${event.feedName}」有新动态：新增 ${event.newCount} 帖，更新 ${event.updatedCount} 帖`;

  if (isTauri) {
    try {
      await runCommand("send_system_notification", { title, body });
      return null;
    } catch (error) {
      return String(error);
    }
  }

  if (!("Notification" in window)) {
    console.warn("当前浏览器不支持系统通知");
    return "当前浏览器环境不支持系统通知";
  }

  // 请求通知权限
  if (Notification.permission === "default") {
    const permission = await Notification.requestPermission();
    if (permission !== "granted") {
      return "浏览器通知权限未授权";
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
    return null;
  }

  return "浏览器通知权限未授权";
}

// 发送一条测试通知（提示音 + 系统通知），走与真实新消息相同的展示逻辑。
// 独立导出：页面里的"测试"按钮无需注册事件监听即可调用；结果直接 toast。
export async function sendCharityTestNotification() {
  playNotificationSound();

  const failure = await sendSystemNotification({
    feedName: "测试订阅源",
    newCount: 1,
    updatedCount: 0,
    timestamp: Math.floor(Date.now() / 1000),
  });
  const { showToast } = useToast();
  if (failure) {
    showToast(failure, true);
  } else {
    showToast("测试系统通知已发送");
  }
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
  void sendSystemNotification(event).then((failure) => {
    if (failure) {
      console.warn("公益监听系统通知未发送：", failure);
    }
  });
}

export function useCharityNotification() {
  let unlisten: (() => void) | null = null;

  onMounted(async () => {
    if (isTauri) {
      const { listen } = await import("@tauri-apps/api/event");

      unlisten = await listen<CharityNewMessageEvent>("charity-new-message", (event) => {
        handleNewMessage(event.payload);
      });

      console.log("公益监听通知已启用");
      // 授权流程由 send_system_notification 命令内处理（首启会弹系统授权框）
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
