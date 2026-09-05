import { onMounted, onUnmounted } from "vue";

interface CharityNewMessageEvent {
  feedName: string;
  newCount: number;
  updatedCount: number;
  timestamp: number;
}

let audioContext: AudioContext | null = null;
let notificationEnabled = true;

// 生成简单的提示音（使用 Web Audio API）
function playNotificationSound() {
  try {
    if (!audioContext) {
      audioContext = new (window.AudioContext || (window as any).webkitAudioContext)();
    }

    const oscillator = audioContext.createOscillator();
    const gainNode = audioContext.createGain();

    oscillator.connect(gainNode);
    gainNode.connect(audioContext.destination);

    // 设置音调（频率）- 使用清脆的提示音
    oscillator.frequency.value = 800; // Hz
    oscillator.type = "sine";

    // 设置音量淡出效果
    gainNode.gain.setValueAtTime(0.3, audioContext.currentTime);
    gainNode.gain.exponentialRampToValueAtTime(0.01, audioContext.currentTime + 0.3);

    // 播放
    oscillator.start(audioContext.currentTime);
    oscillator.stop(audioContext.currentTime + 0.3);
  } catch (error) {
    console.warn("播放提示音失败:", error);
  }
}

// 发送系统通知
async function sendSystemNotification(event: CharityNewMessageEvent) {
  if (!("Notification" in window)) {
    console.warn("浏览器不支持系统通知");
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
    const title = "公益监听 - 新消息提醒";
    const body = `「${event.feedName}」有新动态：新增 ${event.newCount} 条，更新 ${event.updatedCount} 条`;

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

    // 3秒后自动关闭
    setTimeout(() => {
      notification.close();
    }, 5000);
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

    // 请求通知权限（用户首次使用时）
    if ("Notification" in window && Notification.permission === "default") {
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
  };
}
