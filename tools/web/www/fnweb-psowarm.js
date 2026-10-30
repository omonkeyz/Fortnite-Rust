(() => {
  const qs = new URLSearchParams(location.search);
  if (qs.get('fnweb_psowarm') === '0' || typeof GPUAdapter === 'undefined' || !navigator.gpu) return;
  const state = { started: 0, done: 0, failed: 0, ms: 0, keep: [] };
  window.__fnwebPsoWarm = state;
  const data = fetch(new URL('fnweb-psowarm.json', location.href)).then((r) => (r.ok ? r.json() : null)).catch(() => null);
  const warmed = new WeakSet();
  const requestDevice = GPUAdapter.prototype.requestDevice;
  GPUAdapter.prototype.requestDevice = async function (...args) {
    const dev = await requestDevice.apply(this, args);
    if (!warmed.has(dev)) { warmed.add(dev); data.then((j) => { if (j && j.v === 1) warm(dev, j); }); }
    return dev;
  };
  function warm(dev, j) {
    const t0 = performance.now();
    const shaders = new Map(), bgls = new Map(), pls = new Map();
    const scoped = (f) => { dev.pushErrorScope('validation'); let r = null; try { r = f(); } catch (_) { r = null; } dev.popErrorScope().catch(() => {}); return r; };
    const shader = (i) => { if (!shaders.has(i)) shaders.set(i, scoped(() => dev.createShaderModule({ code: j.shaders[i] }))); return shaders.get(i); };
    const bgl = (i) => { if (!bgls.has(i)) bgls.set(i, scoped(() => dev.createBindGroupLayout({ entries: j.bgls[i].map((e) => { const o = { binding: e.binding, visibility: e.visibility }; for (const k of ['buffer', 'sampler', 'texture', 'storageTexture']) if (e[k]) o[k] = e[k]; return o; }) }))); return bgls.get(i); };
    const layout = (l) => { if (l === 'auto') return 'auto'; if (!pls.has(l)) pls.set(l, scoped(() => { const ls = j.pls[l].map((b) => (b == null ? null : bgl(b))); return ls.some((x, k) => x === null && j.pls[l][k] != null) ? null : dev.createPipelineLayout({ bindGroupLayouts: ls }); })); return pls.get(l); };
    const stage = (s) => { const m = shader(s.m); if (!m) return null; const o = { module: m, entryPoint: s.e }; if (s.c) o.constants = s.c; return o; };
    const jobs = j.pipes.slice();
    let inFlight = 0;
    const next = () => {
      while (inFlight < 6 && jobs.length) {
        const p = jobs.shift();
        let pr = null;
        try {
          const lay = layout(p.l);
          if (lay) {
            if (p.k === 'r') {
              const v = stage(p.v); const f = p.f ? stage(p.f) : null;
              if (v && (!p.f || f)) {
                v.buffers = p.v.b || [];
                const d = { layout: lay, vertex: v };
                if (f) { f.targets = p.f.t || []; d.fragment = f; }
                if (p.p) d.primitive = p.p; if (p.d) d.depthStencil = p.d; if (p.ms) d.multisample = p.ms;
                pr = dev.createRenderPipelineAsync(d);
              }
            } else {
              const c = stage(p.c);
              if (c) pr = dev.createComputePipelineAsync({ layout: lay, compute: c });
            }
          }
        } catch (_) { pr = null; }
        if (!pr) { state.failed++; continue; }
        state.started++; inFlight++;
        pr.then((x) => { state.keep.push(x); state.done++; }, () => { state.failed++; }).finally(() => { inFlight--; state.ms = performance.now() - t0; next(); });
      }
    };
    next();
  }
})();
