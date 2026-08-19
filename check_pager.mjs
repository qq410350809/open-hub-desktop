import puppeteer from "puppeteer";

const browser = await puppeteer.launch({
  headless: true,
  executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});
const page = await browser.newPage();
await page.setViewport({ width: 1440, height: 900 });
page.on("console", (msg) => console.log("[console]", msg.type(), msg.text().slice(0, 200)));
page.on("pageerror", (err) => console.log("[pageerror]", err.message));

await page.goto("http://localhost:5173", { waitUntil: "domcontentloaded", timeout: 20000 });
await new Promise(r => setTimeout(r, 8000));

const result = await page.evaluate(() => {
  const btns = [...document.querySelectorAll("button")];
  const hit = btns.find((b) => b.textContent.includes("时段明细"));
  if (!hit) return { found: false, allBtns: btns.map(b => b.textContent.trim()).filter(t => t.includes("查看全部")) };
  hit.click();
  return { found: true, text: hit.textContent.trim() };
});
console.log("BTN:", JSON.stringify(result));
await new Promise(r => setTimeout(r, 1500));

const modal = await page.evaluate(() => {
  const dialog = [...document.querySelectorAll('[role="dialog"]')].find(d => d.textContent.includes("请求健康矩阵明细"));
  if (!dialog) return { dialog: false };
  const footer = dialog.querySelector(".app-table-pagination");
  const rows = dialog.querySelectorAll(".app-table tbody tr").length;
  const allRows = dialog.querySelectorAll(".app-table tbody tr");
  return {
    dialog: true,
    paginationVisible: !!footer,
    paginationHTML: footer ? footer.textContent.trim().slice(0, 200) : null,
    renderedRows: rows,
    firstRowText: allRows[0] ? allRows[0].textContent.trim().slice(0, 100) : null,
    lastRowText: allRows[rows - 1] ? allRows[rows - 1].textContent.trim().slice(0, 100) : null,
  };
});
console.log("MODAL:", JSON.stringify(modal));

await browser.close();
