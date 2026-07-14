const screens = {
  overview: {
    src: "assets/product-overview.png",
    alt: "Harbor Overview showing a healthy running demo project and its service status."
  },
  agents: {
    src: "assets/product-ai-connections.png",
    alt: "Harbor AI Connections showing Claude Code, Claude Desktop, and Codex connection status."
  }
};

const header = document.querySelector(".site-header");
const screenImage = document.querySelector("[data-screen-image]");

function updateHeader() {
  header?.classList.toggle("scrolled", window.scrollY > 10);
}

window.addEventListener("scroll", updateHeader, { passive: true });
updateHeader();

document.querySelectorAll("[data-screen]").forEach((tab) => {
  tab.addEventListener("click", () => {
    const screen = screens[tab.dataset.screen];
    if (!screen || !screenImage) return;
    document.querySelectorAll("[data-screen]").forEach((item) => {
      item.setAttribute("aria-selected", String(item === tab));
    });
    screenImage.src = screen.src;
    screenImage.alt = screen.alt;
  });
});

document.querySelector("[data-copy-brew]")?.addEventListener("click", async (event) => {
  const button = event.currentTarget;
  try {
    await navigator.clipboard.writeText("brew install --cask luke-fairbanks/tap/harbor");
    button.textContent = "Copied";
    window.setTimeout(() => { button.textContent = "Copy"; }, 1600);
  } catch {
    button.textContent = "Select";
    document.querySelector(".brew-command code")?.focus?.();
  }
});
