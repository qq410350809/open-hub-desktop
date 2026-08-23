import { createApp } from "vue";
import "./styles.css";
import App from "./App.vue";
import { loadCapabilities } from "./composables/core/capabilities";

createApp(App).mount("#app");
void loadCapabilities();
