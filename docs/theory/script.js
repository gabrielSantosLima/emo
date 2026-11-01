(() => {
  "use strict";

  const root = document.documentElement;

  /* ---------- Theme ---------- */

  const themeToggle = document.getElementById("theme-toggle");

  function setTheme(theme, persist = true) {
    root.dataset.theme = theme;
    themeToggle?.setAttribute(
      "aria-label",
      theme === "dark" ? "Switch to light theme" : "Switch to dark theme"
    );
    if (persist) {
      try {
        localStorage.setItem("emo-theory-theme", theme);
      } catch {}
    }
  }

  setTheme(root.dataset.theme === "light" ? "light" : "dark", false);

  themeToggle?.addEventListener("click", () => {
    setTheme(root.dataset.theme === "dark" ? "light" : "dark");
  });

  /* ---------- Table of contents ---------- */

  const sections = Array.from(document.querySelectorAll("main section[id]"));
  const tocNav = document.getElementById("toc-nav");
  const tocLinks = [];

  if (tocNav) {
    for (const section of sections) {
      const heading = section.querySelector("h2");
      if (!heading) continue;
      const link = document.createElement("a");
      link.href = `#${section.id}`;
      link.textContent = heading.textContent.trim();
      tocNav.append(link);
      tocLinks.push({ link, section });
    }
  }

  /* ---------- Reading time ---------- */

  const readingTime = document.getElementById("reading-time");
  const main = document.getElementById("main");
  if (readingTime && main) {
    const words = (main.innerText || "").trim().split(/\s+/).filter(Boolean).length;
    readingTime.textContent = `~${Math.max(1, Math.round(words / 220))} min read`;
  }

  /* ---------- Scroll UI: progress, active section, back to top ---------- */

  const progressBar = document.getElementById("progress-bar");
  const backTop = document.getElementById("back-top");
  let scrollQueued = false;

  function updateScrollUI() {
    const scrollTop = window.scrollY || window.pageYOffset;
    const maxScroll = root.scrollHeight - window.innerHeight;

    if (progressBar) {
      progressBar.style.width = `${maxScroll > 0 ? (scrollTop / maxScroll) * 100 : 0}%`;
    }

    if (backTop) {
      backTop.hidden = scrollTop < 600;
    }

    const probe = scrollTop + 140;
    let active = sections[0] || null;
    for (const section of sections) {
      const top = section.getBoundingClientRect().top + scrollTop;
      if (top <= probe) active = section;
    }

    for (const { link, section } of tocLinks) {
      const isActive = active === section;
      link.classList.toggle("active", isActive);
      if (isActive) {
        link.setAttribute("aria-current", "true");
      } else {
        link.removeAttribute("aria-current");
      }
    }
  }

  function requestScrollUpdate() {
    if (scrollQueued) return;
    scrollQueued = true;
    requestAnimationFrame(() => {
      scrollQueued = false;
      updateScrollUI();
    });
  }

  window.addEventListener("scroll", requestScrollUpdate, { passive: true });
  window.addEventListener("resize", requestScrollUpdate);
  updateScrollUI();

  backTop?.addEventListener("click", () => {
    window.scrollTo({ top: 0, behavior: "smooth" });
  });

  /* ---------- Timing explorer ---------- */

  const TIMINGS = {
    ntsc: {
      label: "NTSC",
      master: 21477272,
      cpu: 3579545,
      dot: 5369318,
      lineCycles: 1364,
      lines: 262,
    },
    pal: {
      label: "PAL",
      master: 21281370,
      cpu: 3546895,
      dot: 5320343,
      lineCycles: 1364,
      lines: 312,
    },
  };

  const timingBody = document.getElementById("timing-body");
  const timingNote = document.getElementById("timing-note");
  const timingButtons = document.querySelectorAll("[data-std]");

  function mhz(value) {
    return `${(value / 1e6).toFixed(4)} MHz`;
  }

  function renderTiming(key) {
    const timing = TIMINGS[key];
    if (!timing || !timingBody) return;

    const fps = timing.master / (timing.lineCycles * timing.lines);
    const rows = [
      ["Master clock", mhz(timing.master), "Crystal reference"],
      ["S-CPU", mhz(timing.cpu), `master ÷ ${Math.round(timing.master / timing.cpu)}`],
      ["PPU dot clock", mhz(timing.dot), `master ÷ ${Math.round(timing.master / timing.dot)}`],
      [
        "Scanline",
        `${timing.lineCycles} master cycles`,
        `${timing.lineCycles / 4} dots`,
      ],
      [
        "Frame",
        `${fps.toFixed(4)} Hz`,
        `${timing.lines} lines × ${timing.lineCycles} cycles`,
      ],
      ["SPC700 (APU)", "1.0240 MHz", "independent 24.576 MHz domain"],
      ["S-DSP", "32.000 kHz", "8 voices, stereo"],
    ];

    timingBody.innerHTML = rows
      .map(
        ([unit, rate, derivation]) =>
          `<tr><td>${unit}</td><td class="num">${rate}</td><td>${derivation}</td></tr>`
      )
      .join("");

    if (timingNote) {
      timingNote.textContent = `${timing.label}: ${timing.lines} scanlines per frame, ${timing.lineCycles} master cycles per line. The APU clocks are independent of the video master clock.`;
    }
  }

  timingButtons.forEach((button) => {
    button.addEventListener("click", () => {
      timingButtons.forEach((other) =>
        other.setAttribute("aria-pressed", String(other === button))
      );
      renderTiming(button.dataset.std);
    });
  });

  renderTiming("ntsc");

  /* ---------- Reference filter ---------- */

  const refFilter = document.getElementById("ref-filter");
  const refCount = document.getElementById("ref-count");
  const refEmpty = document.getElementById("ref-empty");
  const refs = Array.from(document.querySelectorAll(".ref"));
  const refGroups = Array.from(document.querySelectorAll(".ref-group"));

  function updateReferences() {
    if (!refFilter) return;

    const query = refFilter.value.trim().toLowerCase();
    let visible = 0;

    for (const ref of refs) {
      const matches =
        query.length === 0 || ref.textContent.toLowerCase().includes(query);
      ref.hidden = !matches;
      if (matches) visible += 1;
    }

    for (const group of refGroups) {
      group.hidden = !Array.from(group.querySelectorAll(".ref")).some(
        (ref) => !ref.hidden
      );
    }

    if (refCount) refCount.textContent = `${visible} / ${refs.length}`;
    if (refEmpty) refEmpty.hidden = visible !== 0;
  }

  refFilter?.addEventListener("input", updateReferences);
  updateReferences();
})();
