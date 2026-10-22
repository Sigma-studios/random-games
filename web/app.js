// Random Games: draws /api/data. Pages: #/ (games), #/joueurs, #/joueur/<name>,
// #/jeu/<slug>, #/historique. Every name comes from the sheet or Discord, so
// all text goes through esc().
"use strict";

const state = {
  data: null,
  include: null, // null: the sheet's checkboxes
  charts: [],
  filters: { q: "", proposer: "", maxPrice: null, minVotes: 0, sort: null },
  history: { action: "", person: "" },
  matrixPhase: "before",
  tables: {}, // chart id -> showing its table
};

// ---------- helpers ----------

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const $ = (sel, root = document) => root.querySelector(sel);
const fmt = (v, digits = 2) =>
  v == null || Number.isNaN(v) ? "–" : Number(v).toLocaleString("fr-FR", { maximumFractionDigits: digits });
const price = (v) => (v == null ? "–" : v === 0 ? "Gratuit" : `${fmt(v)} €`);
const signed = (v) => (v == null ? "–" : `${v > 0 ? "+" : ""}${fmt(v)}`);
const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
const series = () => ["--s1", "--s2", "--s3", "--s4", "--s5", "--s6", "--s7", "--s8"].map(css);
const dateFr = (iso, opts = { day: "numeric", month: "short", year: "numeric" }) =>
  iso ? new Date(iso).toLocaleDateString("fr-FR", opts) : "–";
const mean = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : null);
const round2 = (v) => (v == null ? null : Math.round(v * 100) / 100);

function gameBySlug(slug) {
  return state.data.games.find((g) => g.slug === slug);
}
// Colour follows the person (their order on the sheet), never their rank.
function personColor(name) {
  const i = state.data.people.findIndex((p) => p.name === name);
  return i >= 0 && i < 8 ? series()[i] : css("--muted");
}
function avatarUrl(name) {
  const p = state.data.people.find((p) => p.name === name);
  return p ? p.avatar : null;
}
function person(name, { link = true } = {}) {
  if (!name) return '<span class="muted">–</span>';
  const url = avatarUrl(name);
  const img = url ? `<img src="${esc(url)}" alt="" loading="lazy">` : "";
  const known = state.data.people.some((p) => p.name === name);
  return link && known
    ? `<a class="person" href="#/joueur/${encodeURIComponent(name)}">${img}${esc(name)}</a>`
    : `<span class="person">${img}${esc(name)}</span>`;
}
function gameCell(g) {
  const cover = g.cover
    ? `<img class="cover" src="${esc(g.cover)}" alt="" loading="lazy" onerror="this.replaceWith(Object.assign(document.createElement('span'),{className:'cover blank'}))">`
    : '<span class="cover blank">web</span>';
  return `<div class="game-cell">${cover}<a href="#/jeu/${encodeURIComponent(g.slug)}">${esc(g.name)}</a></div>`;
}
function gradeBar(v) {
  if (v == null) return '<span class="muted">–</span>';
  const { vote_min: lo, vote_max: hi } = state.data;
  const pct = Math.max(0, Math.min(100, ((v - lo) / (hi - lo)) * 100));
  return `<div class="bar-cell"><span class="num">${fmt(v)}</span><span class="track"><span class="fill" style="width:${pct}%"></span></span></div>`;
}
function delta(v) {
  if (v == null) return '<span class="muted">–</span>';
  const cls = v > 0 ? "delta-up" : v < 0 ? "delta-down" : "muted";
  const arrow = v > 0 ? "▲" : v < 0 ? "▼" : "";
  return `<span class="num ${cls}">${arrow} ${esc(signed(v))}</span>`;
}

// ---------- charts ----------

function baseOption() {
  const ink2 = css("--ink-2");
  return {
    animationDuration: 300,
    textStyle: { fontFamily: 'system-ui, -apple-system, "Segoe UI", sans-serif', color: ink2 },
    grid: { left: 8, right: 16, top: 28, bottom: 8, containLabel: true },
    tooltip: {
      backgroundColor: css("--surface"),
      borderColor: css("--ring"),
      textStyle: { color: css("--ink"), fontSize: 13 },
      extraCssText: "box-shadow: 0 4px 16px rgba(0,0,0,.12); border-radius: 10px;",
    },
    legend: { top: 0, left: 0, icon: "circle", itemWidth: 9, itemHeight: 9, textStyle: { color: ink2 } },
  };
}
function axis(extra = {}) {
  return {
    axisLine: { lineStyle: { color: css("--axis") } },
    axisTick: { show: false },
    axisLabel: { color: css("--muted") },
    splitLine: { lineStyle: { color: css("--grid") } },
    ...extra,
  };
}
function mountChart(id, option, onClick) {
  const node = document.getElementById(id);
  if (!node || state.tables[id]) return;
  const chart = echarts.init(node, null, { renderer: "svg" });
  chart.setOption(option);
  if (onClick) chart.on("click", onClick);
  state.charts.push(chart);
}
function disposeCharts() {
  state.charts.forEach((c) => c.dispose());
  state.charts = [];
}
// A chart card with its table twin (every value readable without the chart).
function chartCard(id, title, hint, table, size = "") {
  const showing = !!state.tables[id];
  return `<section class="card">
    <div class="card-head"><h2>${esc(title)}</h2>
      <button class="link view-toggle" data-table-toggle="${id}" type="button">${showing ? "voir le graphique" : "voir en tableau"}</button></div>
    ${hint ? `<p class="hint">${hint}</p>` : ""}
    ${showing ? `<div class="table-wrap">${table()}</div>` : `<div id="${id}" class="chart ${size}" role="img" aria-label="${esc(title)}"></div>`}
  </section>`;
}
function simpleTable(head, rows) {
  return `<table><thead><tr>${head.map((h) => `<th class="${h.num ? "num" : ""}">${esc(h.label)}</th>`).join("")}</tr></thead>
    <tbody>${rows.map((r) => `<tr>${r.map((c, i) => `<td class="${head[i].num ? "num" : ""}">${c}</td>`).join("")}</tr>`).join("")}</tbody></table>`;
}

// ---------- data ----------

async function load() {
  const q = state.include ? `?include=${encodeURIComponent([...state.include].join(","))}` : "";
  const view = $("#view");
  if (state.data) view.style.opacity = "0.6"; // keep the previous render while refetching
  const res = await fetch(`/api/data${q}`);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  state.data = await res.json();
  view.style.opacity = "";
  $("#updated").textContent = `Mis à jour ${new Date(state.data.updated_at).toLocaleString("fr-FR", { dateStyle: "medium", timeStyle: "short" })}`;
  renderWho();
}

function renderWho() {
  const d = state.data;
  $("#who").innerHTML = d.people
    .map((p) => {
      const on = d.included.includes(p.name);
      return `<button class="chip" type="button" data-person="${esc(p.name)}" aria-pressed="${on}">
        <img src="${esc(p.avatar)}" alt="">${esc(p.name)}</button>`;
    })
    .join("");
  const sheetDefault = d.people.every((p) => p.sheet_included === d.included.includes(p.name));
  $("#who-reset").hidden = sheetDefault;
}

// ---------- pages ----------

function route() {
  const [page, arg] = location.hash.replace(/^#\/?/, "").split("/");
  return { page: page || "", arg: arg ? decodeURIComponent(arg) : null };
}

function render() {
  if (!state.data) return;
  disposeCharts();
  const { page, arg } = route();
  document.querySelectorAll("[data-nav]").forEach((a) => {
    const nav = a.dataset.nav;
    a.classList.toggle(
      "active",
      (nav === "home" && (page === "" || page === "jeu")) ||
        (nav === "people" && (page === "joueurs" || page === "joueur")) ||
        (nav === "history" && page === "historique"),
    );
  });
  const pages = { "": home, joueurs: people, joueur: player, jeu: game, historique: history };
  const y = window.scrollY;
  (pages[page] || home)(arg);
  // A new page starts at the top; a re-render (filters, refresh) stays put.
  const here = location.hash;
  window.scrollTo({ top: here === state.lastHash ? y : 0 });
  state.lastHash = here;
}

// ----- Games (the Recap sheet) -----

function filteredGames(list) {
  const f = state.filters;
  const q = f.q.trim().toLowerCase();
  return list
    .map(gameBySlug)
    .filter(Boolean)
    .filter((g) => !q || g.name.toLowerCase().includes(q))
    .filter((g) => !f.proposer || g.proposer === f.proposer)
    .filter((g) => f.maxPrice == null || (g.price ?? 0) <= f.maxPrice)
    .filter((g) => (g.played ? g.votes_after : g.votes_before) >= f.minVotes);
}

function home() {
  const d = state.data;
  const toPlay = filteredGames(d.to_play);
  const played = filteredGames(d.played);
  const gradesGiven = d.games.reduce((n, g) => n + Object.keys(g.before).length + Object.keys(g.after).length, 0);
  const playedAvg = round2(mean(d.games.filter((g) => g.played && g.avg_after != null).map((g) => g.avg_after)));
  const prices = d.games.map((g) => g.price).filter((p) => p != null);
  const maxPrice = Math.ceil(Math.max(10, ...prices));
  const f = state.filters;

  $("#view").innerHTML = `
    <div class="grid kpis">
      ${kpi("Jeux proposés", d.games.length, `${d.games.filter((g) => !g.played).length} pas encore joués`)}
      ${kpi("Jeux joués", d.games.filter((g) => g.played).length, "")}
      ${kpi("Notes données", gradesGiven.toLocaleString("fr-FR"), "avant et après avoir joué")}
      ${kpi("Note moyenne après avoir joué", fmt(playedAvg), "des jeux joués")}
    </div>
    <div class="filters" role="search">
      <label>Chercher<input type="search" id="f-q" value="${esc(f.q)}" placeholder="Nom du jeu"></label>
      <label>Proposé par<select id="f-proposer"><option value="">Tout le monde</option>
        ${d.people.map((p) => `<option ${f.proposer === p.name ? "selected" : ""}>${esc(p.name)}</option>`).join("")}</select></label>
      <label>Prix max : <span id="f-price-v">${f.maxPrice == null ? "tous" : price(f.maxPrice)}</span>
        <input type="range" id="f-price" min="0" max="${maxPrice}" step="1" value="${f.maxPrice ?? maxPrice}"></label>
      <label>Votes min : <span id="f-votes-v">${f.minVotes}</span>
        <input type="range" id="f-votes" min="0" max="${d.people.length}" step="1" value="${f.minVotes}"></label>
    </div>
    <section class="card">
      <div class="card-head"><h2>À jouer ensuite</h2><span class="muted">${toPlay.length} jeux</span></div>
      <p class="hint">Les jeux pas encore joués, par note moyenne avant d'y jouer.</p>
      ${gameTable("toplay", toPlay, [
        { label: "Note", num: true, key: "avg_before", cell: (g) => gradeBar(g.avg_before) },
        { label: "Votes", num: true, key: "votes_before", cell: (g) => g.votes_before },
        { label: "Proposé par", key: "proposer", cell: (g) => person(g.proposer) },
        { label: "Prix", num: true, key: "price", cell: (g) => price(g.price) },
      ])}
    </section>
    <section class="card">
      <div class="card-head"><h2>Jeux joués</h2><span class="muted">${played.length} jeux</span></div>
      <p class="hint">Par note moyenne après y avoir joué, et ce qu'elle a gagné ou perdu par rapport à avant.</p>
      ${gameTable("played", played, [
        { label: "Note après", num: true, key: "avg_after", cell: (g) => gradeBar(g.avg_after) },
        { label: "Avant", num: true, key: "avg_before", cell: (g) => fmt(g.avg_before) },
        { label: "Écart", num: true, key: "diff", cell: (g) => delta(g.diff) },
        { label: "Proposé par", key: "proposer", cell: (g) => person(g.proposer) },
      ])}
    </section>
    <div class="grid two">
      ${chartCard("c-comeback", "Retournements et flops", "Écart entre la note après et la note avant, sur les jeux joués : les 8 plus grandes hausses et baisses.", comebackTable, "tall")}
      ${chartCard("c-price", "Prix et note", "Chaque point est un jeu : sa note (avant d'y jouer s'il n'est pas joué, après sinon) selon son prix.", priceTable, "tall")}
    </div>
    ${chartCard("c-activity", "Activité", activityHint(), activityTable, "short")}
  `;
  bindHomeFilters(maxPrice);
  bindSortable();
  drawComebacks();
  drawPriceScatter();
  drawActivity();
}

function kpi(label, value, note) {
  return `<div class="card kpi"><div class="label">${esc(label)}</div><div class="value">${esc(value)}</div>${note ? `<div class="note">${esc(note)}</div>` : ""}</div>`;
}

const LIMIT = 15;
function gameTable(id, games, cols) {
  if (!games.length) return '<p class="empty">Aucun jeu ne correspond aux filtres.</p>';
  const sort = state.filters.sort?.table === id ? state.filters.sort : null;
  let rows = games;
  if (sort) {
    const col = cols.find((c) => c.key === sort.key);
    rows = [...games].sort((a, b) => {
      const x = a[col.key], y = b[col.key];
      if (x == null) return 1;
      if (y == null) return -1;
      return (typeof x === "string" ? x.localeCompare(y) : x - y) * sort.dir;
    });
  }
  const expanded = state.tables[`more-${id}`];
  const shown = expanded ? rows : rows.slice(0, LIMIT);
  const head = cols
    .map((c) => {
      const s = sort && sort.key === c.key ? (sort.dir > 0 ? "ascending" : "descending") : null;
      return `<th class="sortable ${c.num ? "num" : ""}" data-sort="${id}:${c.key}" ${s ? `aria-sort="${s}"` : ""}>${esc(c.label)}${s ? (sort.dir > 0 ? " ↑" : " ↓") : ""}</th>`;
    })
    .join("");
  return `<div class="table-wrap"><table>
    <thead><tr><th class="rank">#</th><th>Jeu</th>${head}</tr></thead>
    <tbody>${shown
      .map((g, i) => `<tr><td class="rank">${i + 1}</td><td>${gameCell(g)}</td>${cols.map((c) => `<td class="${c.num ? "num" : ""}">${c.cell(g)}</td>`).join("")}</tr>`)
      .join("")}</tbody></table></div>
    ${rows.length > LIMIT ? `<button class="link more" type="button" data-more="${id}">${expanded ? "Voir moins" : `Voir les ${rows.length} jeux`}</button>` : ""}`;
}

function bindSortable() {
  document.querySelectorAll("[data-sort]").forEach((th) =>
    th.addEventListener("click", () => {
      const [table, key] = th.dataset.sort.split(":");
      const cur = state.filters.sort;
      // Descending, then ascending, then back to the table's own order.
      const same = cur && cur.table === table && cur.key === key;
      state.filters.sort = !same ? { table, key, dir: -1 } : cur.dir < 0 ? { table, key, dir: 1 } : null;
      render();
    }),
  );
  document.querySelectorAll("[data-more]").forEach((b) =>
    b.addEventListener("click", () => {
      const k = `more-${b.dataset.more}`;
      state.tables[k] = !state.tables[k];
      render();
    }),
  );
}

function bindHomeFilters(maxPrice) {
  const f = state.filters;
  let t;
  $("#f-q").addEventListener("input", (e) => {
    f.q = e.target.value;
    clearTimeout(t);
    t = setTimeout(() => {
      const pos = e.target.selectionStart;
      render();
      const q = $("#f-q");
      q.focus();
      q.setSelectionRange(pos, pos);
    }, 200);
  });
  $("#f-proposer").addEventListener("change", (e) => {
    f.proposer = e.target.value;
    render();
  });
  $("#f-price").addEventListener("input", (e) => {
    const v = Number(e.target.value);
    $("#f-price-v").textContent = v >= maxPrice ? "tous" : price(v);
  });
  $("#f-price").addEventListener("change", (e) => {
    const v = Number(e.target.value);
    f.maxPrice = v >= maxPrice ? null : v;
    render();
  });
  $("#f-votes").addEventListener("input", (e) => ($("#f-votes-v").textContent = e.target.value));
  $("#f-votes").addEventListener("change", (e) => {
    f.minVotes = Number(e.target.value);
    render();
  });
}

function comebackRows() {
  const all = state.data.comebacks.map(gameBySlug).filter(Boolean);
  const up = all.filter((g) => g.diff > 0).slice(0, 8);
  const down = all.filter((g) => g.diff < 0).slice(-8);
  return [...up, ...down.filter((g) => !up.includes(g))];
}
function comebackTable() {
  return simpleTable(
    [{ label: "Jeu" }, { label: "Avant", num: true }, { label: "Après", num: true }, { label: "Écart", num: true }],
    comebackRows().map((g) => [gameCell(g), fmt(g.avg_before), fmt(g.avg_after), delta(g.diff)]),
  );
}
function drawComebacks() {
  const rows = comebackRows().slice().reverse();
  if (!rows.length) return emptyChart("c-comeback", "Pas encore de jeu noté avant et après.");
  const pos = css("--div-pos"), neg = css("--div-neg");
  const o = baseOption();
  mountChart(
    "c-comeback",
    {
      ...o,
      legend: { show: false },
      grid: { ...o.grid, top: 8 },
      tooltip: {
        ...o.tooltip,
        trigger: "item",
        formatter: (p) => {
          const g = rows[p.dataIndex];
          return `<b>${esc(g.name)}</b><br>${fmt(g.avg_before)} → ${fmt(g.avg_after)} (${esc(signed(g.diff))})`;
        },
      },
      xAxis: axis({ type: "value", splitLine: { lineStyle: { color: css("--grid") } } }),
      yAxis: axis({ type: "category", data: rows.map((g) => g.name), axisLabel: { color: css("--ink-2"), width: 150, overflow: "truncate" } }),
      series: [
        {
          type: "bar",
          barMaxWidth: 14,
          data: rows.map((g) => ({
            value: g.diff,
            itemStyle: { color: g.diff >= 0 ? pos : neg, borderRadius: g.diff >= 0 ? [0, 4, 4, 0] : [4, 0, 0, 4] },
          })),
          label: { show: true, position: "outside", formatter: (p) => signed(p.value), color: css("--ink-2"), fontSize: 11 },
        },
      ],
    },
    (p) => (location.hash = `#/jeu/${encodeURIComponent(rows[p.dataIndex].slug)}`),
  );
}

function priceGames() {
  return filteredGames(state.data.games.map((g) => g.slug)).filter(
    (g) => g.price != null && (g.played ? g.avg_after : g.avg_before) != null,
  );
}
function priceTable() {
  return simpleTable(
    [{ label: "Jeu" }, { label: "Prix", num: true }, { label: "Note", num: true }, { label: "Joué" }],
    priceGames().map((g) => [gameCell(g), price(g.price), fmt(g.played ? g.avg_after : g.avg_before), g.played ? "oui" : "non"]),
  );
}
function drawPriceScatter() {
  const games = priceGames();
  if (!games.length) return emptyChart("c-price", "Aucun jeu avec un prix et une note.");
  const [c1, c2] = series();
  const surface = css("--surface");
  const point = (g) => ({ value: [g.price, g.played ? g.avg_after : g.avg_before], slug: g.slug, name: g.name });
  const o = baseOption();
  const mk = (name, color, data) => ({
    name,
    type: "scatter",
    symbolSize: 10,
    data,
    itemStyle: { color, borderColor: surface, borderWidth: 2, opacity: 0.9 },
    emphasis: { scale: 1.5 },
  });
  mountChart(
    "c-price",
    {
      ...o,
      grid: { ...o.grid, top: 48 },
      tooltip: {
        ...o.tooltip,
        trigger: "item",
        formatter: (p) => `<b>${esc(p.data.name)}</b><br>${esc(price(p.data.value[0]))} · note ${fmt(p.data.value[1])}`,
      },
      xAxis: axis({ type: "value", name: "Prix (€)", nameLocation: "middle", nameGap: 26, nameTextStyle: { color: css("--muted") } }),
      yAxis: axis({ type: "value", min: state.data.vote_min, max: state.data.vote_max, name: "Note", nameTextStyle: { color: css("--muted") } }),
      series: [
        mk("Pas encore joué (note avant)", c1, games.filter((g) => !g.played).map(point)),
        mk("Joué (note après)", c2, games.filter((g) => g.played).map(point)),
      ],
    },
    (p) => (location.hash = `#/jeu/${encodeURIComponent(p.data.slug)}`),
  );
}

// Grades and proposals per week, from the journal the bot keeps.
function weeks() {
  const evs = state.data.events;
  if (!evs.length) return [];
  const start = (d) => {
    const x = new Date(d);
    x.setHours(0, 0, 0, 0);
    x.setDate(x.getDate() - ((x.getDay() + 6) % 7));
    return x;
  };
  const first = start(evs[0].at), last = start(new Date());
  const out = [];
  for (let w = new Date(first); w <= last; w.setDate(w.getDate() + 7)) out.push({ week: new Date(w), grades: 0, added: 0 });
  for (const e of evs) {
    const i = Math.round((start(e.at) - first) / (7 * 864e5));
    if (!out[i]) continue;
    if (e.action === "grade_before" || e.action === "grade_after") out[i].grades++;
    if (e.action === "added") out[i].added++;
  }
  return out;
}
function activityHint() {
  const evs = state.data.events;
  return evs.length
    ? `Notes données et jeux proposés par semaine, depuis le ${esc(dateFr(evs[0].at))} (début du journal).`
    : "Le journal des actions commence avec le nouveau bot : rien encore.";
}
function activityTable() {
  return simpleTable(
    [{ label: "Semaine du" }, { label: "Notes", num: true }, { label: "Jeux proposés", num: true }],
    weeks().map((w) => [esc(dateFr(w.week)), w.grades, w.added]),
  );
}
function drawActivity() {
  const ws = weeks();
  if (!ws.length) return emptyChart("c-activity", "Pas encore d'activité enregistrée.");
  const [c1, c2] = series();
  const o = baseOption();
  mountChart("c-activity", {
    ...o,
    tooltip: { ...o.tooltip, trigger: "axis", axisPointer: { type: "shadow" } },
    xAxis: axis({ type: "category", data: ws.map((w) => dateFr(w.week, { day: "numeric", month: "short" })) }),
    yAxis: axis({ type: "value", minInterval: 1 }),
    series: [
      { name: "Notes données", type: "bar", stack: "a", data: ws.map((w) => w.grades), itemStyle: { color: c1 }, barMaxWidth: 18 },
      { name: "Jeux proposés", type: "bar", stack: "b", data: ws.map((w) => w.added), itemStyle: { color: c2, borderRadius: [4, 4, 0, 0] }, barMaxWidth: 18 },
    ],
  });
}

function emptyChart(id, text) {
  const node = document.getElementById(id);
  if (node) node.outerHTML = `<p class="empty">${esc(text)}</p>`;
}

// ----- People (Recap A1:H7 and the Stats from Simon matrices) -----

function people() {
  const d = state.data;
  $("#view").innerHTML = `
    <div class="page-title"><h1>Joueurs</h1></div>
    <section class="card">
      <div class="card-head"><h2>Récap</h2></div>
      <p class="hint">Note reçue : la note moyenne des jeux qu'ils ont proposés (selon les personnes cochées en haut). Note donnée : la moyenne de toutes les notes qu'ils ont mises.</p>
      <div class="table-wrap">${recapTable()}</div>
    </section>
    <section class="card">
      <div class="card-head"><h2>Qui note qui</h2>
        <div class="seg" role="group" aria-label="Notes">
          <button type="button" data-phase="before" aria-pressed="${state.matrixPhase === "before"}">Avant d'y jouer</button>
          <button type="button" data-phase="after" aria-pressed="${state.matrixPhase === "after"}">Après</button>
        </div></div>
      <p class="hint">Note moyenne que chaque personne (en ligne) donne aux jeux proposés par chaque autre (en colonne).</p>
      ${state.tables["c-matrix"] ? `<div class="table-wrap">${matrixTable()}</div>` : `<div id="c-matrix" class="chart tall" role="img" aria-label="Qui note qui"></div>`}
      <button class="link view-toggle" data-table-toggle="c-matrix" type="button">${state.tables["c-matrix"] ? "voir le graphique" : "voir en tableau"}</button>
    </section>
    <div class="grid two">
      ${chartCard("c-spread", "Comment chacun note", "Répartition des notes données par chacun, avant et après avoir joué : la boîte va du premier au troisième quartile, le trait est la médiane.", spreadTable, "tall")}
      ${chartCard("c-proposals", "Propositions", "Jeux proposés par chacun, et combien ont été joués.", proposalsTable, "tall")}
    </div>`;
  document.querySelectorAll("[data-phase]").forEach((b) =>
    b.addEventListener("click", () => {
      state.matrixPhase = b.dataset.phase;
      render();
    }),
  );
  drawMatrix();
  drawSpread();
  drawProposals();
}

function recapTable() {
  const rows = state.data.summaries;
  return `<table><thead><tr><th>Joueur</th>
      <th class="num">Proposés</th><th class="num">Joués</th>
      <th class="num">Reçue avant</th><th class="num">Reçue après</th><th class="num">Reçue</th>
      <th class="num">Donnée avant</th><th class="num">Donnée après</th><th class="num">Donnée</th></tr></thead>
    <tbody>${rows
      .map(
        (s) => `<tr><td>${person(s.name)}${s.included ? "" : ' <span class="tag">pas compté</span>'}</td>
      <td class="num">${s.proposed}</td><td class="num">${s.proposed_played}</td>
      <td class="num">${fmt(s.proposed_avg_before)}</td><td class="num">${fmt(s.proposed_avg_after)}</td><td class="num"><b>${fmt(s.proposed_avg)}</b></td>
      <td class="num">${fmt(s.given_avg_before)}</td><td class="num">${fmt(s.given_avg_after)}</td><td class="num"><b>${fmt(s.given_avg)}</b></td></tr>`,
      )
      .join("")}</tbody></table>`;
}

function matrix() {
  return state.matrixPhase === "before" ? state.data.matrix_before : state.data.matrix_after;
}
function matrixTable() {
  const names = state.data.people.map((p) => p.name);
  const m = matrix();
  return simpleTable(
    [{ label: "Note donnée par ↓ à →" }, ...names.map((n) => ({ label: n, num: true }))],
    names.map((giver) => [person(giver), ...names.map((p) => fmt(m[giver]?.[p]))]),
  );
}
function drawMatrix() {
  const names = state.data.people.map((p) => p.name);
  const m = matrix();
  const data = [];
  names.forEach((giver, y) =>
    names.forEach((proposer, x) => {
      const v = m[giver]?.[proposer];
      if (v != null) data.push([x, y, v]);
    }),
  );
  const o = baseOption();
  const seq = ["--seq-1", "--seq-2", "--seq-3", "--seq-4", "--seq-5", "--seq-6", "--seq-7"].map(css);
  const values = data.map((d) => d[2]);
  const lo = Math.floor(Math.min(...values, state.data.vote_max)), hi = Math.ceil(Math.max(...values, state.data.vote_min));
  // Dark ink on the light half of the ramp, white on the dark half.
  const light = document.documentElement.dataset.theme === "dark" || (!document.documentElement.dataset.theme && matchMedia("(prefers-color-scheme: dark)").matches)
    ? (v) => v > lo + (hi - lo) * 0.6
    : (v) => v < lo + (hi - lo) * 0.5;
  const cells = data.map((d) => ({ value: d, label: { color: light(d[2]) ? "#0b0b0b" : "#ffffff" } }));
  mountChart("c-matrix", {
    ...o,
    legend: { show: false },
    grid: { left: 8, right: 8, top: 8, bottom: 56, containLabel: true },
    tooltip: {
      ...o.tooltip,
      trigger: "item",
      formatter: (p) => `<b>${esc(names[p.value[1]])}</b> → jeux de <b>${esc(names[p.value[0]])}</b><br>note moyenne ${fmt(p.value[2])}`,
    },
    xAxis: axis({ type: "category", data: names, position: "top", splitLine: { show: false }, axisLine: { show: false }, axisLabel: { color: css("--ink-2") } }),
    yAxis: axis({ type: "category", data: names, inverse: true, splitLine: { show: false }, axisLine: { show: false }, axisLabel: { color: css("--ink-2") } }),
    visualMap: {
      min: lo,
      max: hi,
      calculable: false,
      orient: "horizontal",
      left: "center",
      bottom: 0,
      itemWidth: 12,
      itemHeight: 160,
      inRange: { color: seq },
      textStyle: { color: css("--muted") },
      text: ["haute", "basse"],
    },
    series: [
      {
        type: "heatmap",
        data: cells,
        label: { show: true, formatter: (p) => fmt(p.value[2], 1), fontSize: 12 },
        itemStyle: { borderColor: css("--surface"), borderWidth: 2, borderRadius: 6 },
      },
    ],
  });
}

function quartiles(xs) {
  const s = [...xs].sort((a, b) => a - b);
  const q = (p) => {
    const i = (s.length - 1) * p, lo = Math.floor(i), hi = Math.ceil(i);
    return s[lo] + (s[hi] - s[lo]) * (i - lo);
  };
  return s.length ? [s[0], q(0.25), q(0.5), q(0.75), s[s.length - 1]] : null;
}
function gradesOf(name, phase) {
  return state.data.games.map((g) => (phase === "before" ? g.before : g.after)[name]).filter((v) => v != null);
}
function spreadTable() {
  return simpleTable(
    [{ label: "Joueur" }, { label: "Notes avant", num: true }, { label: "Médiane avant", num: true }, { label: "Notes après", num: true }, { label: "Médiane après", num: true }],
    state.data.people.map((p) => {
      const b = quartiles(gradesOf(p.name, "before")), a = quartiles(gradesOf(p.name, "after"));
      return [person(p.name), gradesOf(p.name, "before").length, fmt(b?.[2]), gradesOf(p.name, "after").length, fmt(a?.[2])];
    }),
  );
}
function drawSpread() {
  const names = state.data.people.map((p) => p.name);
  const [c1, c2] = series();
  const o = baseOption();
  const box = (phase) => names.map((n) => quartiles(gradesOf(n, phase)) ?? "-");
  const style = (c) => ({ color: `${c}33`, borderColor: c, borderWidth: 2 });
  mountChart("c-spread", {
    ...o,
    tooltip: {
      ...o.tooltip,
      trigger: "item",
      formatter: (p) => {
        const [, lo, q1, med, q3, hi] = p.data;
        return `<b>${esc(p.name)}</b> · ${esc(p.seriesName)}<br>médiane ${fmt(med)} · de ${fmt(q1)} à ${fmt(q3)}<br>extrêmes ${fmt(lo)} – ${fmt(hi)}`;
      },
    },
    xAxis: axis({ type: "category", data: names, axisLabel: { color: css("--ink-2"), interval: 0 } }),
    yAxis: axis({ type: "value", min: state.data.vote_min, max: state.data.vote_max }),
    series: [
      { name: "Avant d'y jouer", type: "boxplot", data: box("before"), itemStyle: style(c1), boxWidth: [8, 18] },
      { name: "Après", type: "boxplot", data: box("after"), itemStyle: style(c2), boxWidth: [8, 18] },
    ],
  });
}
function proposalsTable() {
  return simpleTable(
    [{ label: "Joueur" }, { label: "Proposés", num: true }, { label: "Joués", num: true }],
    state.data.summaries.map((s) => [person(s.name), s.proposed, s.proposed_played]),
  );
}
function drawProposals() {
  const s = state.data.summaries;
  const [c1, c2] = series();
  const o = baseOption();
  mountChart("c-proposals", {
    ...o,
    tooltip: { ...o.tooltip, trigger: "axis", axisPointer: { type: "shadow" } },
    xAxis: axis({ type: "category", data: s.map((x) => x.name), axisLabel: { color: css("--ink-2"), interval: 0 } }),
    yAxis: axis({ type: "value", minInterval: 1 }),
    series: [
      { name: "Proposés", type: "bar", data: s.map((x) => x.proposed), itemStyle: { color: c1, borderRadius: [4, 4, 0, 0] }, barMaxWidth: 22, barGap: "12%" },
      { name: "Joués", type: "bar", data: s.map((x) => x.proposed_played), itemStyle: { color: c2, borderRadius: [4, 4, 0, 0] }, barMaxWidth: 22 },
    ],
  });
}

// ----- One player (Stats from Simon) -----

function player(name) {
  const d = state.data;
  const st = d.players.find((p) => p.name === name);
  if (!st) return notFound("Ce joueur n'existe pas.");
  const sum = d.summaries.find((s) => s.name === name);
  const theirs = d.games.filter((g) => g.proposer === name);
  $("#view").innerHTML = `
    <div class="page-title"><img class="hero-avatar" src="${esc(avatarUrl(name))}" alt="">
      <div><h1>${esc(name)}</h1><div class="sub">${sum.proposed} jeux proposés, ${sum.proposed_played} joués · donne en moyenne ${fmt(sum.given_avg_before)} avant, ${fmt(sum.given_avg_after)} après</div></div></div>
    ${chartCard("c-exchange", `Entre ${name} et les autres`, `Avant d'y jouer : la note moyenne que chacun met aux jeux de ${esc(name)}, et celle que ${esc(name)} met aux leurs.`, () => exchangeTable(st), "")}
    <div class="extremes" style="margin-top:16px">
      ${extremeCard("Sa plus haute note, jeu pas joué", st.highest_before)}
      ${extremeCard("Sa plus haute note, jeu joué", st.highest_after)}
      ${extremeCard("Meilleur retournement", st.best_comeback, true)}
      ${extremeCard("Sa plus basse note, jeu pas joué", st.lowest_before)}
      ${extremeCard("Sa plus basse note, jeu joué", st.lowest_after)}
      ${extremeCard("Pire flop", st.worst_flop, true)}
    </div>
    <div class="grid two">
      ${chartCard("c-hist", "Ses notes", `Combien de fois ${esc(name)} a donné chaque note (arrondie), avant et après avoir joué.`, () => histTable(name), "")}
      <section class="card"><div class="card-head"><h2>Ses propositions</h2><span class="muted">${theirs.length} jeux</span></div>
        ${gameTable(`by-${name}`, theirs, [
          { label: "Avant", num: true, key: "avg_before", cell: (g) => fmt(g.avg_before) },
          { label: "Après", num: true, key: "avg_after", cell: (g) => fmt(g.avg_after) },
          { label: "Joué", key: "played", cell: (g) => (g.played ? "oui" : '<span class="muted">non</span>') },
        ])}</section>
    </div>`;
  bindSortable();
  drawExchange(st);
  drawHist(name);
}

function extremeCard(title, ex, signedValue = false) {
  if (!ex) return `<section class="card extreme"><h2>${esc(title)}</h2><p class="empty">–</p></section>`;
  const list = ex.games.slice(0, 6);
  return `<section class="card extreme"><h2>${esc(title)}</h2>
    <div class="value">${signedValue ? delta(ex.value) : esc(fmt(ex.value))}</div>
    <ul>${list
      .map((g) => {
        const full = gameBySlug(g.slug);
        return `<li><a href="#/jeu/${encodeURIComponent(g.slug)}">${esc(full ? full.name : g.name)}</a> <span class="muted">· ${esc(g.proposer)}</span></li>`;
      })
      .join("")}${ex.games.length > list.length ? `<li class="muted">et ${ex.games.length - list.length} autres</li>` : ""}</ul></section>`;
}

function exchangeTable(st) {
  return simpleTable(
    [{ label: "Avec" }, { label: "Il/elle reçoit, avant", num: true }, { label: "après", num: true }, { label: "Il/elle donne, avant", num: true }, { label: "après", num: true }],
    st.exchanges.filter((e) => e.other !== st.name).map((e) => [person(e.other), fmt(e.received_before), fmt(e.received_after), fmt(e.given_before), fmt(e.given_after)]),
  );
}
function drawExchange(st) {
  const rows = st.exchanges.filter((e) => e.other !== st.name);
  const [c1, c2] = series();
  const o = baseOption();
  mountChart("c-exchange", {
    ...o,
    tooltip: { ...o.tooltip, trigger: "axis", axisPointer: { type: "shadow" }, valueFormatter: (v) => fmt(v) },
    xAxis: axis({ type: "category", data: rows.map((e) => e.other), axisLabel: { color: css("--ink-2"), interval: 0 } }),
    yAxis: axis({ type: "value", min: state.data.vote_min, max: state.data.vote_max }),
    series: [
      { name: `Note des autres aux jeux de ${st.name}`, type: "bar", data: rows.map((e) => e.received_before), itemStyle: { color: c1, borderRadius: [4, 4, 0, 0] }, barMaxWidth: 22, barGap: "12%" },
      { name: `Note de ${st.name} à leurs jeux`, type: "bar", data: rows.map((e) => e.given_before), itemStyle: { color: c2, borderRadius: [4, 4, 0, 0] }, barMaxWidth: 22 },
    ],
  });
}
function histBins(name, phase) {
  const { vote_min: lo, vote_max: hi } = state.data;
  const bins = [];
  for (let v = Math.ceil(lo); v <= Math.floor(hi); v++) bins.push(0);
  for (const g of gradesOf(name, phase)) {
    const i = Math.round(g) - Math.ceil(lo);
    if (bins[i] != null) bins[i]++;
  }
  return bins;
}
function histTable(name) {
  const b = histBins(name, "before"), a = histBins(name, "after");
  const lo = Math.ceil(state.data.vote_min);
  return simpleTable(
    [{ label: "Note", num: true }, { label: "Avant", num: true }, { label: "Après", num: true }],
    b.map((n, i) => [i + lo, n, a[i]]),
  );
}
function drawHist(name) {
  const lo = Math.ceil(state.data.vote_min);
  const b = histBins(name, "before"), a = histBins(name, "after");
  const [c1, c2] = series();
  const o = baseOption();
  mountChart("c-hist", {
    ...o,
    tooltip: { ...o.tooltip, trigger: "axis", axisPointer: { type: "shadow" } },
    xAxis: axis({ type: "category", data: b.map((_, i) => i + lo) }),
    yAxis: axis({ type: "value", minInterval: 1 }),
    series: [
      { name: "Avant", type: "bar", data: b, itemStyle: { color: c1, borderRadius: [4, 4, 0, 0] }, barGap: "12%" },
      { name: "Après", type: "bar", data: a, itemStyle: { color: c2, borderRadius: [4, 4, 0, 0] } },
    ],
  });
}

// ----- One game -----

function game(slug) {
  const g = gameBySlug(slug);
  if (!g) return notFound("Ce jeu n'est pas (ou plus) dans la feuille.");
  const evs = state.data.events.filter((e) => e.slug === slug).slice().reverse();
  const names = state.data.people.map((p) => p.name);
  $("#view").innerHTML = `
    <div class="game-hero">
      ${g.cover ? `<img src="${esc(g.cover)}" alt="">` : ""}
      <div>
        <h1 style="margin:0 0 4px;font-size:28px">${esc(g.name)}</h1>
        ${g.link ? `<a class="link" href="${esc(g.link)}" target="_blank" rel="noopener">${g.cover ? "Page Steam" : "Lien"} ↗</a>` : ""}
        <dl class="facts">
          <dt>Proposé par</dt><dd>${person(g.proposer)}</dd>
          ${g.added_at ? `<dt>Ajouté le</dt><dd>${esc(dateFr(g.added_at))}</dd>` : ""}
          <dt>Prix</dt><dd>${esc(price(g.price))}</dd>
          <dt>Joué</dt><dd>${g.played ? `oui${g.played_at ? `, le ${esc(dateFr(g.played_at))}` : ""}` : "pas encore"}</dd>
          ${g.players ? `<dt>Par</dt><dd>${g.players.map((p) => person(p)).join(" ")}</dd>` : ""}
          <dt>Note avant</dt><dd>${fmt(g.avg_before)} <span class="muted">(${g.votes_before} votes)</span></dd>
          ${g.played ? `<dt>Note après</dt><dd>${fmt(g.avg_after)} <span class="muted">(${g.votes_after} votes)</span> ${delta(g.diff)}</dd>` : ""}
        </dl>
      </div>
    </div>
    ${chartCard("c-game", "Les notes", "La note de chacun, avant et après y avoir joué.", () =>
      simpleTable(
        [{ label: "Joueur" }, { label: "Avant", num: true }, { label: "Après", num: true }],
        names.map((n) => [person(n), fmt(g.before[n]), fmt(g.after[n])]),
      ),
    )}
    <section class="card"><div class="card-head"><h2>Historique</h2></div>
      ${evs.length ? `<ul class="events">${evs.map(eventItem).join("")}</ul>` : '<p class="empty">Rien dans le journal pour ce jeu (il commence avec le nouveau bot).</p>'}
    </section>`;
  const [c1, c2] = series();
  const o = baseOption();
  mountChart("c-game", {
    ...o,
    tooltip: { ...o.tooltip, trigger: "axis", axisPointer: { type: "shadow" }, valueFormatter: (v) => fmt(v) },
    xAxis: axis({ type: "category", data: names, axisLabel: { color: css("--ink-2"), interval: 0 } }),
    yAxis: axis({ type: "value", min: state.data.vote_min, max: state.data.vote_max }),
    series: [
      { name: "Avant", type: "bar", data: names.map((n) => g.before[n] ?? null), itemStyle: { color: c1, borderRadius: [4, 4, 0, 0] }, barMaxWidth: 26, barGap: "12%" },
      { name: "Après", type: "bar", data: names.map((n) => g.after[n] ?? null), itemStyle: { color: c2, borderRadius: [4, 4, 0, 0] }, barMaxWidth: 26 },
    ],
  });
}

// ----- History (the Journal sheet) -----

const ACTIONS = {
  added: "a proposé",
  grade_before: "a noté (avant)",
  grade_after: "a noté (après)",
  played: "joué",
  price: "prix",
};
function eventItem(e) {
  const game = e.slug ? `<a href="#/jeu/${encodeURIComponent(e.slug)}">${esc(e.game)}</a>` : esc(e.game);
  let what;
  switch (e.action) {
    case "added":
      what = `${person(e.person)} a proposé ${game}`;
      break;
    case "grade_before":
    case "grade_after":
      what = `${person(e.person)} a ${e.previous != null ? "changé sa note" : "noté"} ${game} ${e.action === "grade_after" ? "après y avoir joué" : ""} : <b class="num">${fmt(e.value)}</b>${e.previous != null ? ` <span class="muted">(avant ${fmt(e.previous)})</span>` : ""}`;
      break;
    case "played":
      what = `${game} a été joué${e.players.length ? ` par ${e.players.map((p) => person(p)).join(" ")}` : ""}`;
      break;
    case "price":
      what = `Le prix de ${game} est passé à ${esc(price(e.value))}${e.previous != null ? ` <span class="muted">(${esc(price(e.previous))})</span>` : ""}`;
      break;
    default:
      what = game;
  }
  return `<li><time datetime="${esc(e.at)}">${esc(new Date(e.at).toLocaleString("fr-FR", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }))}</time><div>${what}</div></li>`;
}

function history() {
  const d = state.data;
  const h = state.history;
  const evs = d.events
    .filter((e) => !h.action || e.action === h.action)
    .filter((e) => !h.person || e.person === h.person || e.players.includes(h.person))
    .slice()
    .reverse();
  const shown = state.tables["more-history"] ? evs : evs.slice(0, 100);
  $("#view").innerHTML = `
    <div class="page-title"><h1>Historique</h1><span class="sub">${evs.length} actions</span></div>
    <div class="filters">
      <label>Action<select id="h-action"><option value="">Toutes</option>
        ${Object.entries(ACTIONS).map(([k, v]) => `<option value="${k}" ${h.action === k ? "selected" : ""}>${esc(v)}</option>`).join("")}</select></label>
      <label>Personne<select id="h-person"><option value="">Tout le monde</option>
        ${d.people.map((p) => `<option ${h.person === p.name ? "selected" : ""}>${esc(p.name)}</option>`).join("")}</select></label>
    </div>
    <section class="card">
      ${shown.length ? `<ul class="events">${shown.map(eventItem).join("")}</ul>` : '<p class="empty">Rien pour l\'instant : le journal commence avec le nouveau bot.</p>'}
      ${evs.length > shown.length ? `<button class="link more" type="button" data-more="history">Tout voir (${evs.length})</button>` : ""}
    </section>`;
  $("#h-action").addEventListener("change", (e) => {
    h.action = e.target.value;
    render();
  });
  $("#h-person").addEventListener("change", (e) => {
    h.person = e.target.value;
    render();
  });
  bindSortable();
}

function notFound(text) {
  $("#view").innerHTML = `<p class="empty">${esc(text)} <a href="#/">Retour aux jeux</a></p>`;
}

// ---------- wiring ----------

document.addEventListener("click", (e) => {
  const chip = e.target.closest("[data-person]");
  if (chip) {
    const name = chip.dataset.person;
    const set = new Set(state.data.included);
    set.has(name) ? set.delete(name) : set.add(name);
    if (!set.size) return; // averages need someone
    state.include = set;
    load().then(render, showError);
    return;
  }
  const toggle = e.target.closest("[data-table-toggle]");
  if (toggle) {
    const id = toggle.dataset.tableToggle;
    state.tables[id] = !state.tables[id];
    render();
  }
});
$("#who-reset").addEventListener("click", () => {
  state.include = null;
  load().then(render, showError);
});
$("#theme").addEventListener("click", () => {
  const dark = matchMedia("(prefers-color-scheme: dark)").matches;
  const cur = document.documentElement.dataset.theme || (dark ? "dark" : "light");
  const next = cur === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem("theme", next); } catch (err) {}
  render();
});
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", render);
window.addEventListener("hashchange", render);
let resizeTimer;
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => state.charts.forEach((c) => c.resize()), 100);
});

function showError(err) {
  $("#view").innerHTML = `<p class="empty">Impossible de charger les données (${esc(err.message)}). <button class="link" onclick="location.reload()">Réessayer</button></p>`;
}

load().then(render, showError);
// The sheet is re-read every two minutes; so is the page, quietly.
setInterval(() => {
  if (document.visibilityState === "visible") load().then(render).catch(() => {});
}, 120000);
