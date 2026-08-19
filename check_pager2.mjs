import puppeteer from "puppeteer";

const browser = await puppeteer.launch({
  headless: true,
  executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});
const page = await browser.newPage();
await page.setViewport({ width: 1440, height: 900 });
await page.goto("http://localhost:5173", { waitUntil: "domcontentloaded", timeout: 20000 });
await new Promise(r => setTimeout(r, 8000));

await page.evaluate(() => {
  const btns = [...document.querySelectorAll("button")];
  const hit = btns.find((b) => b.textContent.includes("时段明细"));
  if (hit) hit.click();
});
await new Promise(r => setTimeout(r, 1500));

// 截取弹窗区域
const dialog = await page.$('.tt-modal-card.is-wide[role="dialog"]');
if (dialog) {
  await dialog.screenshot({ path: "/tmp/health-modal.png" });
  console.log("screenshot saved");
}
await browser.close();
