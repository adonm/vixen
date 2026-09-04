// Shared JS throughput workload: runs unmodified in QuickJS (spike)
// and in V8 (vixen-headless --eval). Reports one number.
let s = 0;
for (let i = 1; i <= 2000000; i++) { s += Math.sqrt(i) % 7; }
let t = '';
for (let i = 0; i < 20000; i++) { t += 'x' + (i % 10); }
let objs = [];
for (let i = 0; i < 50000; i++) { objs.push({i: i, s: 'k' + i}); }
let n = 0;
for (const o of objs) { n += o.i + o.s.length; }
[Math.floor(s), t.length, n].join(':');
