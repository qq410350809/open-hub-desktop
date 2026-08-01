const puppeteer = require('puppeteer');
(async () => {
  const browser = await puppeteer.launch();
  const page = await browser.newPage();
  await page.setViewport({ width: 375, height: 812 }); // iPhone X
  await page.goto('http://localhost:1420', { waitUntil: 'networkidle0' });
  
  // evaluate layout
  const data = await page.evaluate(() => {
    const cards = Array.from(document.querySelectorAll('.site-card'));
    if (!cards.length) return { error: 'No cards found' };
    
    const card = cards[0];
    const rect = card.getBoundingClientRect();
    const style = window.getComputedStyle(card);
    
    const grid = document.querySelector('.site-grid');
    const gridStyle = window.getComputedStyle(grid);
    
    return {
      card: {
        width: rect.width,
        height: rect.height,
        display: style.display,
        minHeight: style.minHeight,
        maxHeight: style.maxHeight,
        overflow: style.overflow,
        html: card.outerHTML.substring(0, 500)
      },
      grid: {
        display: gridStyle.display,
        gridAutoRows: gridStyle.gridAutoRows,
        gridTemplateRows: gridStyle.gridTemplateRows,
        alignContent: gridStyle.alignContent
      }
    };
  });
  console.log(JSON.stringify(data, null, 2));
  await browser.close();
})();
