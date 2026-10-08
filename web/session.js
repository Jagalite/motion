// Serialize acknowledged updates; an uncertain network retry must reuse the exact event.
export class ViewingSession {
  constructor(api, profile, session) { this.api=api; this.profile=profile; this.state=session; this.queue=Promise.resolve(); this.failure=null; }
  update(position, status, file = null) {
    const run=async()=>{
      if(this.failure)throw this.failure;
      if(['ended','stopped'].includes(this.state.status)){if(file)throw new Error('Playback session has ended');return this.state;}
      const event={sequence:this.state.sequence+1,position_seconds:position,status};
      if(file)event.file={file_id:file.file_id,file_revision:file.revision};
      const options={method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(event)};
      const path=`/api/v1/profiles/${encodeURIComponent(this.profile)}/playback-sessions/${encodeURIComponent(this.state.id)}`;
      try {
        try {this.state=await this.api(path,options);}
        catch(error){if(error.status)throw error;this.state=await this.api(path,options);}
        return this.state;
      } catch(error){if(!file||!error.status||error.status>=500)this.failure=error;throw error;}
    };
    const result=this.queue.then(run);this.queue=result.catch(()=>{});return result;
  }
}
export function preferredTrack(tracks,languages) {
  for(const language of languages){const wanted=language.toLowerCase();const found=tracks.find(t=>t.language?.toLowerCase()===wanted)||tracks.find(t=>t.language?.toLowerCase().split('-')[0]===wanted.split('-')[0]);if(found)return found;}
  return null;
}
