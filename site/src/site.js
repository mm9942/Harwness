'use strict';
for(const button of document.querySelectorAll('[data-copy]')){
 button.addEventListener('click',async()=>{const target=document.getElementById(button.dataset.copy);if(!target)return;try{await navigator.clipboard.writeText(target.textContent);button.textContent='Copied';setTimeout(()=>button.textContent='Copy',1800)}catch{button.textContent='Select to copy';const range=document.createRange();range.selectNodeContents(target);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range)}});
}
const list=document.getElementById('crate-list');
if(list){
 let crates=[],selected='harw-types';
 const search=document.getElementById('crate-search'),domain=document.getElementById('domain-filter'),detail=document.getElementById('crate-detail');
 const esc=s=>String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
 function chips(names,optional=[]){return names.length?names.map(n=>`<button type="button" data-crate="${esc(n)}" class="${optional.includes(n)?'optional':''}">${esc(n)}${optional.includes(n)?' ◇':''}</button>`).join(''):'<span class="none">None in this local package graph.</span>'}
 function show(n,focus=false){
  const c=crates.find(x=>x.name===n);if(!c)return;selected=n;
  const optional=c.edges.filter(e=>e.optional&&e.kind!=='dev-dependencies').map(e=>e.name),dev=c.edges.filter(e=>e.kind==='dev-dependencies').map(e=>e.name);
  detail.innerHTML=`<div class="eyebrow">${esc(c.domain.toUpperCase())} / LEVEL ${c.level}</div><h2>${esc(c.name)}</h2><p>${esc(c.summary)}</p><div class="detail-stats"><span><strong>${c.deps.length}</strong>dependencies</span><span><strong>${c.consumers.length}</strong>direct consumers</span><span><strong>${c.transitive_consumers.length}</strong>transitive consumers</span></div><h3>Depends on</h3><div class="dependency-chips">${chips(c.deps,optional)}</div>${optional.length?'<p class="subtle-note">◇ Optional dependency; requires the relevant Cargo feature.</p>':''}<h3>Direct consumers</h3><div class="dependency-chips">${chips(c.consumers)}</div><details class="detail-disclosure"><summary>All ${c.transitive_consumers.length} transitive consumers</summary><div class="dependency-chips">${chips(c.transitive_consumers)}</div></details><details class="detail-disclosure"><summary>Development dependencies & modules</summary><h3>Local dev dependencies</h3><div class="dependency-chips">${chips(dev)}</div><h3>Top-level modules</h3><p>${esc(c.modules.join(' · ')||'Single module or facade')}</p></details><div class="source-note">${esc(c.path)}/Cargo.toml<br>${c.source_files} Rust source files in src/ · source relationships, not runtime activation</div>`;
  for(const b of list.querySelectorAll('button'))b.setAttribute('aria-pressed',String(b.dataset.crate===n));
  history.replaceState(null,'',`#${encodeURIComponent(n)}`);
  if(focus&&matchMedia('(max-width:800px)').matches)detail.scrollIntoView({block:'start',behavior:matchMedia('(prefers-reduced-motion:reduce)').matches?'instant':'smooth'});
 }
 function render(){const q=search.value.trim().toLowerCase(),d=domain.value;const visible=crates.filter(c=>(d==='all'||c.domain===d)&&(c.name.includes(q)||c.summary.toLowerCase().includes(q)));list.innerHTML=visible.length?visible.map(c=>`<button type="button" class="crate-item" data-crate="${esc(c.name)}" aria-pressed="${c.name===selected}">${esc(c.name)}<small>L${c.level} · ${esc(c.domain)} · ${c.consumers.length} direct consumers</small></button>`).join(''):'<p class="empty-list">No matching packages. Try another name or domain.</p>';document.getElementById('result-count').textContent=`${visible.length} / ${crates.length}`;}
 document.addEventListener('click',event=>{const button=event.target.closest('[data-crate]');if(button)show(button.dataset.crate,true)});
 search.addEventListener('input',render);domain.addEventListener('change',render);
 fetch('/crates.json').then(r=>{if(!r.ok)throw Error('Unavailable');return r.json()}).then(data=>{crates=data;const hash=decodeURIComponent(location.hash.slice(1));if(crates.some(c=>c.name===hash))selected=hash;render();show(selected)}).catch(()=>{list.innerHTML='<p class="empty-list">Package data could not be loaded. Reload this page to try again.</p>';detail.innerHTML='<h2>The architecture overview is available below.</h2>';document.getElementById('result-count').textContent='Unavailable';});
}
