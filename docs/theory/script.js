(() => {
  "use strict";

  const root = document.documentElement;
  const LANG = root.lang.toLowerCase().startsWith("pt") ? "pt" : "en";

  const STRINGS = {
    en: {
      themeToLight: "Switch to light theme",
      themeToDark: "Switch to dark theme",
      readingTime: (minutes) => `~${minutes} min read`,
      masterClock: "Master clock",
      scpu: "S-CPU",
      ppuDot: "PPU dot clock",
      scanline: "Scanline",
      masterCycles: "master cycles",
      dots: "dots",
      frame: "Frame",
      lines: "lines",
      cycles: "cycles",
      crystalReference: "Crystal reference",
      masterDiv: "master ÷",
      apuDomain: "independent 24.576 MHz domain",
      dspDesc: "8 voices, stereo",
      timingNote: (label, lines, lineCycles) =>
        `${label}: ${lines} scanlines per frame, ${lineCycles} master cycles per line. The APU clocks are independent of the video master clock.`,
    },
    pt: {
      themeToLight: "Mudar para o tema claro",
      themeToDark: "Mudar para o tema escuro",
      readingTime: (minutes) => `~${minutes} min de leitura`,
      masterClock: "Relógio mestre",
      scpu: "S-CPU",
      ppuDot: "Relógio de dot da PPU",
      scanline: "Linha de varredura",
      masterCycles: "ciclos mestre",
      dots: "dots",
      frame: "Quadro",
      lines: "linhas",
      cycles: "ciclos",
      crystalReference: "Referência do cristal",
      masterDiv: "mestre ÷",
      apuDomain: "domínio independente de 24,576 MHz",
      dspDesc: "8 vozes, estéreo",
      timingNote: (label, lines, lineCycles) =>
        `${label}: ${lines} linhas por quadro, ${lineCycles} ciclos mestre por linha. Os clocks da APU são independentes do clock mestre de vídeo.`,
    },
  };

  const t = STRINGS[LANG];

  /* ---------- Theme ---------- */

  const themeToggle = document.getElementById("theme-toggle");

  function setTheme(theme, persist = true) {
    root.dataset.theme = theme;
    themeToggle?.setAttribute(
      "aria-label",
      theme === "dark" ? t.themeToLight : t.themeToDark
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

  /* ---------- Language switch keeps the current section ---------- */

  document.querySelectorAll(".lang-link").forEach((link) => {
    link.addEventListener("click", () => {
      const base = link.getAttribute("href").split("#")[0];
      link.setAttribute("href", base + window.location.hash);
    });
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
    readingTime.textContent = t.readingTime(
      Math.max(1, Math.round(words / 220))
    );
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
  const numberFormat = new Intl.NumberFormat(
    LANG === "pt" ? "pt-BR" : "en-US",
    { minimumFractionDigits: 4, maximumFractionDigits: 4 }
  );

  function mhz(value) {
    return `${numberFormat.format(value / 1e6)} MHz`;
  }

  function renderTiming(key) {
    const timing = TIMINGS[key];
    if (!timing || !timingBody) return;

    const fps = timing.master / (timing.lineCycles * timing.lines);
    const rows = [
      [t.masterClock, mhz(timing.master), t.crystalReference],
      [t.scpu, mhz(timing.cpu), `${t.masterDiv} ${Math.round(timing.master / timing.cpu)}`],
      [t.ppuDot, mhz(timing.dot), `${t.masterDiv} ${Math.round(timing.master / timing.dot)}`],
      [
        t.scanline,
        `${timing.lineCycles} ${t.masterCycles}`,
        `${timing.lineCycles / 4} ${t.dots}`,
      ],
      [
        t.frame,
        `${numberFormat.format(fps)} Hz`,
        `${timing.lines} ${t.lines} × ${timing.lineCycles} ${t.cycles}`,
      ],
      ["SPC700 (APU)", "1.0240 MHz", t.apuDomain],
      ["S-DSP", "32.000 kHz", t.dspDesc],
    ];

    timingBody.innerHTML = rows
      .map(
        ([unit, rate, derivation]) =>
          `<tr><td>${unit}</td><td class="num">${rate}</td><td>${derivation}</td></tr>`
      )
      .join("");

    if (timingNote) {
      timingNote.textContent = t.timingNote(
        timing.label,
        timing.lines,
        timing.lineCycles
      );
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

    for (const group of refGroups) {
      const groupTitle = (
        group.querySelector("h3")?.textContent || ""
      ).toLowerCase();
      let groupVisible = false;

      for (const ref of group.querySelectorAll(".ref")) {
        const matches =
          query.length === 0 ||
          groupTitle.includes(query) ||
          ref.textContent.toLowerCase().includes(query);
        ref.hidden = !matches;
        if (matches) {
          visible += 1;
          groupVisible = true;
        }
      }

      group.hidden = !groupVisible;
    }

    if (refCount) refCount.textContent = `${visible} / ${refs.length}`;
    if (refEmpty) refEmpty.hidden = visible !== 0;
  }

  refFilter?.addEventListener("input", updateReferences);
  updateReferences();
})();
