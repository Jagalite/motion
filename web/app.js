import {ViewingSession, preferredTrack} from '/session.js';
import {createDemuxePlayer} from '/demuxe.js';
import {attemptPlayback, preparationSubmitter, sourceForPlayback, PlaybackSwitches, transferPlayback, transferTracks, intendsPlay, playbackSnapshot, NativeHandoffs} from '/playback.js';
const $ = id => document.getElementById(id);
let offset = 0, total = 0, playerElement, current, unsubscribe, lastSave = 0, opening = false, changing = false, playbackProfile;
const limit = 24;
let session=null, preferences=null, nextEpisode=null, refreshTimer, playbackPlan=null, playbackSource=null;
async function api(path, options = {}) {
  const response = await fetch(path, {...options,signal:options.signal?AbortSignal.any([options.signal,AbortSignal.timeout(15000)]):AbortSignal.timeout(15000)});
  if (!response.ok) { const error = await response.json().catch(() => ({})); const failure=new Error(error.message || `Request failed (${response.status})`);failure.status=response.status;throw failure; }
  return response.json();
}
const submitPreparation=preparationSubmitter(body=>api('/api/v1/processing-jobs',{method:'POST',headers:adminHeaders(),body:JSON.stringify(body)}));
const mutation = (method, body) => ({method, headers: {'Content-Type':'application/json'}, body: JSON.stringify(body)});
async function browse() {
  try {
    const page = await api(`/api/v1/items?limit=${limit}&offset=${offset}&q=${encodeURIComponent($('search').value)}`);
    total = page.total; $('items').replaceChildren();
    for (const item of page.items) {
      const button = document.createElement('button'); button.className='card';
      const kind=document.createElement('span'); kind.className='type'; kind.textContent=item.kind.toUpperCase();
      const title=document.createElement('strong'); title.textContent=item.title;
      const meta=document.createElement('small'); meta.textContent=item.available ? `${item.edition_label} · ${(item.bytes/1048576).toFixed(1)} MB` : 'Original unavailable · check versions';
      button.append(kind,title,meta); button.onclick=()=>openItem(item).catch(showPlaybackError); $('items').append(button);
    }
    $('status').textContent=total ? `${total} files in your collection` : 'Your library is empty. Add a media folder below, then scan it.';
    $('previous').disabled=offset===0; $('next').disabled=offset+limit>=total;
    $('page').textContent=total ? `${offset+1}–${Math.min(offset+limit,total)} of ${total}` : '0 items';
  } catch(error) { $('status').textContent=error.message; }
}
function showPlaybackError(error) { $('playback-status').textContent=error.message; }
async function save(status) {
  if (!session || !playerElement?.player || opening) return;
  const owner=session,element=playerElement,state=element.player.state;
  if (!Number.isFinite(state.currentTime)) return;
  await owner.update(state.currentTime,status||(['playing','ended'].includes(state.status)?state.status:'paused'));
  if(owner!==session||element!==playerElement)return;
  $('playback-status').textContent=`Position saved at ${Math.floor(state.currentTime)}s`;
}
const switches=new PlaybackSwitches();
const nativeHandoffs=new NativeHandoffs();
let pendingPlan=null;
async function closePlayer() {
  switches.cancel(); pendingPlan=null;
  unsubscribe?.(); unsubscribe=null;
  await save('stopped').catch(showPlaybackError);
  session=null;
  try{await dispose(playerElement);}finally{
    playerElement=null; current=null; playbackPlan=null; playbackSource=null; playbackProfile=null; nextEpisode=null; $('viewing').hidden=true;
  }
}
async function loadPreferences() {
  preferences=await api(`/api/v1/profiles/${encodeURIComponent($('profile').value)}/playback-preferences`);
  const p=preferences.preferences;
  $('audio-languages').value=p.audio_languages.join(', ');$('subtitle-languages').value=p.subtitle_languages.join(', ');
  $('subtitle-mode').value=p.subtitle_mode;$('quality').value=p.quality;$('conversion-recipe').value=p.conversion_recipe;
}
async function applyPreferences(core,p) {
  const audio=preferredTrack(core.state.audioTracks,p.audio_languages);
  if(audio)await core.selectAudioTrack(audio.id);
  const language=core.state.audioTracks.find(t=>t.selected)?.language;
  const familiar=language && p.audio_languages.some(l=>l.toLowerCase().split('-')[0]===language.toLowerCase().split('-')[0]);
  const enabled=p.subtitle_mode==='always'||(p.subtitle_mode==='foreign_audio'&&!familiar);
  const subtitles=preferredTrack(core.state.subtitleTracks,p.subtitle_languages);
  if(enabled&&subtitles)await core.selectSubtitleTrack(subtitles.id);
  await core.subtitleVisible(enabled);
}
async function continueWatching() {
  const profile=$('profile').value;
  const page=await api(`/api/v1/profiles/${encodeURIComponent(profile)}/continue-watching?limit=24`);
  if(profile!==$('profile').value)return;
  $('continue-items').replaceChildren();$('continue-section').hidden=!page.items.length;
  for(const item of page.items){const button=document.createElement('button');button.className='card';button.textContent=`${item.title} · Resume at ${Math.floor(item.position_seconds)}s`;button.disabled=!item.available;button.onclick=()=>api(`/api/v1/items/${encodeURIComponent(item.item_id)}`).then(openItem).catch(showPlaybackError);$('continue-items').append(button);}
}
async function playNext() {
  if(!nextEpisode?.available)return;
  const owner=session,next=nextEpisode;
  const item=await api(`/api/v1/items/${encodeURIComponent(next.item_id)}`);
  if(owner!==session||next!==nextEpisode||changing)return;
  await openItem(item,undefined,true);
}
async function openItem(item,mediaUrl,autoplay=false,override=null) {
  if(changing)return; changing=true;
  setPlaybackBusy(true);
  try {
  await closePlayer(); opening=true;
  $('viewing').hidden=false; current=item; playbackProfile=$('profile').value; $('playing-title').textContent=item.title; $('playback-status').textContent='Planning…'; $('playback-plan').textContent=''; $('prepare-playback').hidden=true; $('check-version').hidden=true; $('next-episode').hidden=true;
    const choices=await api(`/api/v1/items/${encodeURIComponent(item.id)}/playback-options`);$('version').replaceChildren();
    await loadPreferences();
    const versions=[...choices.originals.map(v=>({label:v.edition_label,...v})),...choices.renditions];
    const mode=override?.mode||preferences.preferences.quality;
    const recipe=override?.recipe||preferences.preferences.conversion_recipe;
    $('playback-mode').value=mode; $('playback-recipe').value=recipe;
    const explicit=mediaUrl ? versions.find(v=>v.media_url===mediaUrl) : null;
    if(mediaUrl&&!explicit)throw new Error('The selected version is no longer available');
    playbackSource=sourceForPlayback(item,explicit,override?.source);
    const selected=explicit&&!(mode==='convert'&&!explicit.source_file_id) ? {file_id:explicit.file_id,revision:explicit.file_revision||explicit.revision} : undefined;
    for(const choice of versions) {const option=document.createElement('option');option.value=choice.media_url;option.textContent=choice.label;option.disabled=!choice.available||(mode==='original'&&!choices.originals.some(v=>v.file_id===choice.file_id));$('version').append(option);}
    if(mediaUrl)$('version').value=mediaUrl;
    let core, lastOpenError;
    playbackPlan=await attemptPlayback({
      getPlan:failed_versions=>api(`/api/v1/profiles/${encodeURIComponent($('profile').value)}/items/${encodeURIComponent(item.id)}/playback-plan`,mutation('POST',{mode,recipe,selected,source:playbackSource,failed_versions})),
      open:async selection=>{
        playerElement=await createDemuxePlayer();
        $('player-host').replaceChildren(playerElement); core=await playerElement.ready;
        await playerElement.open(selection.media_url);
      },
      onFailure:async error=>{
        lastOpenError=error;
        $('playback-status').textContent=`Could not open this version: ${error.message}`;
        if(playerElement){await playerElement.destroy();playerElement.remove();playerElement=null;}
      }
    });
    renderPlan(playbackPlan);
    if(playbackPlan.status!=='ready') {
      if(lastOpenError)$('playback-status').textContent+=` Last attempt: ${lastOpenError.message}`;
      if(playbackPlan.preparation?.job_id){
        const task=switches.begin(),jobId=playbackPlan.preparation.job_id;
        setTimeout(()=>waitForPreparation(jobId,task).then(()=>{task.check();return openItem(item,undefined,autoplay,{mode,recipe,source:playbackSource});}).catch(error=>{if(task.current)showPlaybackError(error);}).finally(()=>task.finish()),0);
      }
      opening=false; return;
    }
    mediaUrl=playbackPlan.selection.media_url; $('version').value=mediaUrl;
    playbackProfile=$('profile').value;
    const profilePath=`/api/v1/profiles/${encodeURIComponent(playbackProfile)}`;
    const view=await api(`${profilePath}/viewing/${encodeURIComponent(item.id)}`);
    const choice=playbackPlan.selection;
    const started=await api(`${profilePath}/playback-sessions`,mutation('POST',{item_id:item.id,file_id:choice.file_id,file_revision:choice.revision,expected_revision:view.revision}));
    session=new ViewingSession(api,playbackProfile,started);
    if(started.position_seconds>0&&!view.watched)await core.seek(started.position_seconds);
    await applyPreferences(core,preferences.preferences);
    nextEpisode=(await api(`${profilePath}/next-episode/${encodeURIComponent(item.id)}`).catch(error=>{if(error.status===400)return {next:null};throw error;})).next;
    $('next-episode').hidden=!nextEpisode;$('next-episode').disabled=!nextEpisode?.available;
    opening=false; lastSave=Date.now();
    observePlayer(core);
    $('playback-status').textContent='Ready. Press Play to begin.';
    if(autoplay)await core.play();
    $('viewing').scrollIntoView({behavior:'smooth'});
  } catch(error) { opening=false; showPlaybackError(new Error(`Playback unavailable: ${error.message}`)); }
  finally {changing=false;setPlaybackBusy(false);}
}
const playbackOverride=()=>({mode:$('playback-mode').value,recipe:$('playback-recipe').value,source:playbackSource});
function setPlaybackBusy(busy){for(const id of ['close','profile','version','playback-mode','playback-recipe','prepare-playback','check-version','mark-watched','mark-unwatched'])$(id).disabled=busy;$('next-episode').disabled=busy||!nextEpisode?.available;}
function renderPlan(plan){
  const labels={original:'Original delivery',remux:'MP4 remux',audio_conversion:'Audio conversion',video_transcode:'Video transcode',unknown:'Prepared version'};
  $('prepare-playback').hidden=plan.status!=='preparation_required'||!!plan.preparation?.job_id;
  $('check-version').hidden=plan.status!=='preparation_required';
  if(plan.status==='ready')$('playback-plan').textContent=`${labels[plan.selection.operation]} · ${plan.selection.delivery==='original'?'Original file':'Existing version'}${plan.selection.client_support==='unknown'?' · Compatibility checked by the player':''}`;
  else if(plan.status==='preparation_required'){
    $('playback-plan').textContent=`${labels[plan.preparation.operation]} · ${plan.preparation.job_id?'A conversion is queued or running. Playback will switch when it is ready.':'Preparation requires the library admin token below.'} ${plan.warnings.join(' ')}`;
    $('playback-status').textContent=session?'Current playback continues while this version prepares.':'This version must finish preparing before it can play.';
  }else{
    const reasons={prepared_version_unusable:'The prepared version could not play or exceeds the requested bitrate. Choose another conversion or version.',original_unavailable_or_unsupported:'The original is unavailable or could not play. Choose Auto or Convert to try another path.',selected_version_unavailable:'The selected version cannot be used with this mode. Choose another version or mode.',no_processable_source:'No playable version or available source for conversion was found.',processing_backend_unavailable:'The selected encoder does not support this conversion.'};
    $('playback-plan').textContent=reasons[plan.reason]||plan.reason;
    $('playback-status').textContent=session?'Requested switch unavailable. Current playback continues.':'Playback is blocked.';
  }
}
function observePlayer(core) {
  unsubscribe?.();
  let previousStatus=core.state.status, recoveredError=false;
  unsubscribe=core.subscribe(state=>{
    const stopped=state.status!==previousStatus&&['paused','ended'].includes(state.status);previousStatus=state.status;
    if (!opening && ['playing','paused','ended'].includes(state.status) && (stopped||Date.now()-lastSave>5000)) {
      lastSave=Date.now(); save().then(()=>{if(playerElement?.player===core&&!changing&&state.status==='ended'&&$('autoplay').checked)return playNext();}).catch(showPlaybackError);
    }
    if(!opening&&state.status==='error'&&$('playback-mode').value==='auto'&&!switches.pending&&!recoveredError){
      recoveredError=true;
      switchPlayback(undefined,[{file_id:playbackPlan.selection.file_id,revision:playbackPlan.selection.revision}]).catch(showPlaybackError);
    }
  });
}

const disposals=new WeakMap();
function dispose(element) {
  if(!element)return Promise.resolve();
  if(!disposals.has(element)){disposals.set(element,(async()=>{try{await element.destroy();}finally{element.remove();}})());}
  return disposals.get(element);
}
async function switchPlayback(mediaUrl, failed=[]) {
  if(changing||!current)return;
  if(!session)return openItem(current,mediaUrl,false,playbackOverride());
  const task=switches.begin(), item=current, profile=playbackProfile, activeSession=session;
  const mode=$('playback-mode').value,recipe=$('playback-recipe').value;
  let candidate,releaseCandidate=()=>{};
  const completedJobs=new Set();
  pendingPlan=null;
  $('prepare-playback').hidden=true;$('check-version').hidden=true;
  $('playback-status').textContent='Preparing the switch. Current playback continues…';
  try {
    const choices=await api(`/api/v1/items/${encodeURIComponent(item.id)}/playback-options`,{signal:task.signal});task.check();
    const versions=[...choices.originals,...choices.renditions];
    const explicit=mediaUrl?versions.find(v=>v.media_url===mediaUrl):null;
    if(mediaUrl&&!explicit)throw new Error('The selected version is no longer available');
    const source=sourceForPlayback(item,explicit,playbackSource);
    const selected=explicit&&!(mode==='convert'&&!explicit.source_file_id)?{file_id:explicit.file_id,revision:explicit.file_revision||explicit.revision}:undefined;
    for(;;){
      const plan=await api(`/api/v1/profiles/${encodeURIComponent(profile)}/items/${encodeURIComponent(item.id)}/playback-plan`,{...mutation('POST',{mode,recipe,source,selected,failed_versions:failed}),signal:task.signal});task.check();
      pendingPlan=plan;renderPlan(plan);
      if(plan.status==='preparation_required') {
        if(!plan.preparation.job_id)return;
        if(completedJobs.has(plan.preparation.job_id))throw new Error('The completed conversion is not available for playback. Current playback has been kept.');
        await waitForPreparation(plan.preparation.job_id,task);task.check();completedJobs.add(plan.preparation.job_id);continue;
      }
      if(plan.status!=='ready')return;
      const choice=plan.selection;
      if(choice.file_id===playbackPlan.selection.file_id&&choice.revision===playbackPlan.selection.revision){playbackSource=source;playbackPlan=plan;pendingPlan=null;$('playback-status').textContent='Current version already matches.';return;}
      candidate=await createDemuxePlayer();task.check();candidate.classList.add('staging-player');
      const staged=candidate;
      releaseCandidate=task.onCancel(()=>{void dispose(staged).catch(()=>{});});
      $('player-host').append(candidate);
      const core=await candidate.ready;task.check();
      try {
        await candidate.open(choice.media_url);task.check();
        await core.setMuted(true);
        await applyPreferences(core,preferences.preferences);task.check();
        const active=playbackSnapshot(playerElement.player);
        await transferTracks(active,core);
        await core.setPlaybackRate(active.playbackRate);
        await core.seek(playbackSnapshot(playerElement.player).currentTime);task.check();
        if(intendsPlay(playerElement.player.state))await core.play();
      } catch(error) {
        releaseCandidate();await dispose(candidate);candidate=null;task.check();
        failed.push({file_id:choice.file_id,revision:choice.revision});
        if(failed.length>=8)throw error;
        continue;
      }
      task.check();
      if(['ended','stopped'].includes(activeSession.state.status)||playerElement.player.state.status==='ended')return;
      // Only the short acknowledged handoff is locked. Planning/encoding remain cancellable.
      changing=true;setPlaybackBusy(true);opening=true;
      const old=playerElement, oldChoice=playbackPlan.selection, focused=document.activeElement===playerElement;
      try {
        const native=choice.source_file_id&&choice.source_file_id===oldChoice.source_file_id?nativeHandoffs:null;
        await transferPlayback({from:old.player,to:core,session:activeSession,selection:choice,previous:oldChoice,check:()=>task.check(),native,activate:()=>{
          releaseCandidate();playerElement=candidate;candidate=null;
          old.hidden=true;playerElement.classList.remove('staging-player');
        }});
        if(focused){playerElement.tabIndex=0;playerElement.focus({preventScroll:true});}
        playbackPlan=plan;playbackSource=source;pendingPlan=null;
        observePlayer(core);lastSave=Date.now();
        // Add newly published conversions to the version selector.
        const option=document.createElement('option');option.value=choice.media_url;option.textContent=choice.label;
        if(![...$('version').options].some(o=>o.value===choice.media_url))$('version').append(option);
        $('version').value=choice.media_url;
        renderPlan(plan);$('playback-status').textContent='Switched. Playback position preserved.';
        await dispose(old).catch(()=>{});
      } finally {opening=false;changing=false;setPlaybackBusy(false);}
      return;
    }
  } catch(error) {if(task.current)showPlaybackError(error);}
  finally {releaseCandidate();try{await dispose(candidate);}catch(error){if(task.current)showPlaybackError(error);}finally{task.finish();}}
}
async function waitForPreparation(id,task) {
  for(;;){
    task.check();
    const job=await api(`/api/v1/processing-jobs/${encodeURIComponent(id)}`,{signal:task.signal});task.check();
    if(job.phase==='completed'&&!job.expired)return;
    if(!['queued','running'].includes(job.phase))throw new Error(`Preparation ${job.expired?'expired':job.phase}. Current playback has been kept.`);
    $('playback-status').textContent=`Preparing version (${job.phase}). Current playback continues…`;
    await task.delay(1000);
  }
}
$('version').onchange=()=>switchPlayback($('version').value).catch(showPlaybackError);
for(const id of ['playback-mode','playback-recipe'])$(id).onchange=()=>switchPlayback().catch(showPlaybackError);
$('check-version').onclick=()=>switchPlayback().catch(showPlaybackError);
$('prepare-playback').onclick=async()=>{
  const plan=pendingPlan||playbackPlan;
  if(changing||!current||!plan?.preparation)return;
  const proposal=plan.preparation, item=current, profile=playbackProfile;
  if(!$('token').value){showPlaybackError(new Error('Enter the library admin token below to prepare this version.'));return;}
  $('prepare-playback').disabled=true;
  try{
    const job=await submitPreparation(proposal);
    if(current!==item||playbackProfile!==profile||(pendingPlan||playbackPlan)!==plan)return;
    if(['failed','cancelled'].includes(job.phase)||job.expired)throw new Error(`Preparation ${job.expired?'expired':job.phase}. Try preparing again.`);
    proposal.job_id=job.id;
    if(session)await switchPlayback();
    else {
      const task=switches.begin();
      try{await waitForPreparation(job.id,task);task.check();await openItem(item,undefined,false,playbackOverride());}finally{task.finish();}
    }
    await processingJobs();
  }catch(error){if(current===item&&playbackProfile===profile&&(pendingPlan||playbackPlan)===plan)showPlaybackError(error);}finally{if(!changing)$('prepare-playback').disabled=false;}
};
async function changePlaybackView(action){
  if(changing)return;
  changing=true;setPlaybackBusy(true);
  try{return await action();}finally{changing=false;setPlaybackBusy(false);}
}
$('close').onclick=()=>changePlaybackView(closePlayer).catch(showPlaybackError);
$('profile').onchange=()=>changePlaybackView(async()=>{
  switches.cancel();pendingPlan=null;
  if(!session||!playerElement){await closePlayer();await Promise.all([loadPreferences(),continueWatching()]);return;}
  const previousProfile=playbackProfile, previousSession=session, profile=$('profile').value;
  const path=`/api/v1/profiles/${encodeURIComponent(profile)}`;
  try {
    const [view,prefs]=await Promise.all([api(`${path}/viewing/${encodeURIComponent(current.id)}`),api(`${path}/playback-preferences`)]);
    const choice=playbackPlan.selection;
    opening=true;
    const started=await api(`${path}/playback-sessions`,mutation('POST',{item_id:current.id,file_id:choice.file_id,file_revision:choice.revision,expected_revision:view.revision}));
    const replacement=new ViewingSession(api,profile,started),state=playerElement.player.state;
    await replacement.update(state.currentTime,state.status==='ended'?'ended':intendsPlay(state)?'playing':'paused');
    session=replacement;playbackProfile=profile;preferences=prefs;
    await previousSession.update(state.currentTime,'stopped').catch(showPlaybackError);
    await applyPreferences(playerElement.player,prefs.preferences);
    await Promise.all([loadPreferences(),continueWatching()]);
    nextEpisode=(await api(`${path}/next-episode/${encodeURIComponent(current.id)}`).catch(()=>({next:null}))).next;
    $('next-episode').hidden=!nextEpisode;$('next-episode').disabled=!nextEpisode?.available;
    $('playback-status').textContent='Viewer changed. Playback continues at the current position.';
  } catch(error) {
    // Once attribution has changed, do not point the UI back to the old viewer.
    $('profile').value=playbackProfile||previousProfile;throw error;
  } finally {opening=false;}
}).then(()=>{
  if(session){$('playback-mode').value=preferences.preferences.quality;$('playback-recipe').value=preferences.preferences.conversion_recipe;return switchPlayback();}
}).catch(showPlaybackError);
$('search-form').onsubmit=event=>{event.preventDefault();offset=0;browse();};
$('previous').onclick=()=>{offset=Math.max(0,offset-limit);browse();};
$('next').onclick=()=>{offset+=limit;browse();};
async function libraries() {
  const rows=await api('/api/v1/libraries'); $('libraries').replaceChildren();
  for(const row of rows){const div=document.createElement('div'), label=document.createElement('span'), button=document.createElement('button');label.textContent=row.name;button.textContent='Scan';button.onclick=()=>scan(row.id).catch(e=>$('admin-status').textContent=e.message);const interval=document.createElement('input');interval.type='number';interval.min='0';interval.placeholder='Scan every N minutes';interval.setAttribute('aria-label',`${row.name} scan interval in minutes`);const schedule=document.createElement('button');schedule.textContent='Set schedule';schedule.onclick=()=>api(`/api/v1/admin/scan-schedules/${row.id}`,{method:'PUT',headers:adminHeaders(),body:JSON.stringify({interval_seconds:Number(interval.value)*60})}).then(()=>$('admin-status').textContent=Number(interval.value)?'Scan schedule saved.':'Scan schedule disabled.').catch(e=>$('admin-status').textContent=e.message);div.append(label,button,interval,schedule);$('libraries').append(div);}
}
const adminHeaders=()=>({'Content-Type':'application/json','Authorization':`Bearer ${$('token').value}`});
async function scan(id) {
  const job=await api(`/api/v1/libraries/${encodeURIComponent(id)}/scans`,{method:'POST',headers:adminHeaders()});
  $('admin-status').textContent='Scan queued…';
  for(;;){await new Promise(r=>setTimeout(r,500));const state=await api(`/api/v1/jobs/${job.id}`);$('admin-status').textContent=`Scan ${state.phase}`;if(!['queued','running','cancelling'].includes(state.phase)){await browse();break;}}
}
$('add-library').onsubmit=async event=>{event.preventDefault();try{const row=await api('/api/v1/libraries',{method:'POST',headers:adminHeaders(),body:JSON.stringify({name:$('library-name').value,root:$('library-root').value})});await libraries();await scan(row.id);}catch(error){$('admin-status').textContent=error.message;}};
try { const profiles=await api('/api/v1/profiles');for(const profile of profiles){const option=document.createElement('option');option.value=profile.id;option.textContent=profile.name;$('profile').append(option);} await Promise.all([browse(),libraries(),loadPreferences(),continueWatching()]); } catch(error){$('status').textContent=error.message;}

$('next-episode').onclick=()=>playNext().catch(showPlaybackError);
$('preferences-form').onsubmit=async event=>{event.preventDefault();try{
  const languages=id=>$(id).value.split(',').map(s=>s.trim()).filter(Boolean);
  preferences=await api(`/api/v1/profiles/${encodeURIComponent($('profile').value)}/playback-preferences`,mutation('PUT',{expected_revision:preferences.revision,preferences:{audio_languages:languages('audio-languages'),subtitle_languages:languages('subtitle-languages'),subtitle_mode:$('subtitle-mode').value,quality:$('quality').value,conversion_recipe:$('conversion-recipe').value}}));
  if(playerElement?.player)await applyPreferences(playerElement.player,preferences.preferences);
  $('preferences-status').textContent='Preferences saved.';
}catch(error){$('preferences-status').textContent=error.message;}};
async function setWatched(value){if(changing||!current||!playbackProfile)return;return changePlaybackView(async()=>{const item=current,profile=playbackProfile;await closePlayer();const path=`/api/v1/profiles/${encodeURIComponent(profile)}/viewing/${encodeURIComponent(item.id)}`;const view=await api(path);await api(path,mutation('PUT',{expected_revision:view.revision,watched:value}));await continueWatching();});}
$('mark-watched').onclick=()=>setWatched(true).catch(showPlaybackError);
$('mark-unwatched').onclick=()=>setWatched(false).catch(showPlaybackError);
$('convert').onclick=async()=>{try{if(!current)throw new Error('Choose a title first');const choices=await api(`/api/v1/items/${current.id}/playback-options`);const source=choices.originals.find(v=>v.available);if(!source)throw new Error('An original source is required');const job=await api('/api/v1/processing-jobs',{method:'POST',headers:adminHeaders(),body:JSON.stringify({source_file_id:source.file_id,source_revision:source.revision,recipe:$('recipe').value,backend:$('backend').value,idempotency_key:crypto.randomUUID()})});$('admin-status').textContent=`Conversion queued: ${job.id}`;await processingJobs();}catch(error){$('admin-status').textContent=error.message;}};
async function processingJobs(){const jobs=await api('/api/v1/processing-jobs');$('processing-jobs').replaceChildren();for(const job of jobs.slice(0,10)){const row=document.createElement('div'),text=document.createElement('span');text.textContent=`${job.recipe} · ${job.phase} · ${Math.floor(job.progress_seconds)}s${job.expired?' · cache expired':''}${job.error?' · '+job.error:''}`;row.append(text);if(['queued','running','failed','cancelled'].includes(job.phase)){const button=document.createElement('button');const action=['failed','cancelled'].includes(job.phase)?'retry':'cancel';button.textContent=action;button.onclick=()=>api(`/api/v1/processing-jobs/${job.id}/control`,{method:'POST',headers:adminHeaders(),body:JSON.stringify({action})}).then(processingJobs).catch(e=>$('admin-status').textContent=e.message);row.append(button);}$('processing-jobs').append(row);}}
$('clean-cache').onclick=()=>api('/api/v1/admin/cache',{method:'POST',headers:adminHeaders()}).then(v=>$('admin-status').textContent=`Cache: ${(v.bytes/1048576).toFixed(1)} MB. Removed ${v.removed_jobs} expired or failed jobs.`).catch(e=>$('admin-status').textContent=e.message);
const changes=new EventSource('/api/v1/events');
function refresh(){if(refreshTimer)return;refreshTimer=setTimeout(()=>{refreshTimer=null;Promise.all([browse(),continueWatching(),processingJobs()]).catch(showPlaybackError);},1500);}
changes.addEventListener('change',refresh);changes.addEventListener('reset',refresh);
document.addEventListener('visibilitychange',()=>{if(document.hidden)save().catch(showPlaybackError);});
processingJobs().catch(showPlaybackError);
