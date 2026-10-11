/* Motion design gallery: shared mock library + procedural original artwork.
   Every poster and backdrop is generated here as SVG; no external images. */
(function () {
  'use strict';

  // ---------- seeded randomness ----------
  function hash(str) {
    let h = 2166136261;
    for (let i = 0; i < str.length; i++) { h ^= str.charCodeAt(i); h = Math.imul(h, 16777619); }
    return h >>> 0;
  }
  function rng(seed) {
    let a = seed >>> 0;
    return function () {
      a = (a + 0x6d2b79f5) >>> 0;
      let t = a;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  // ---------- library ----------
  const P = (name, role) => ({ name, role });
  const L = [
    {
      id: 'salt-meridian', title: 'The Salt Meridian', type: 'movie', year: 2024, rating: 'PG-13', runtime: 128,
      genres: ['Drama', 'Adventure'], director: 'Ines Valcárcel', studio: 'Lowtide Pictures',
      tagline: 'Every coastline is a border someone drew.',
      synopsis: 'A cartographer hired to settle a dispute over a vanishing salt marsh walks the line herself, and finds the villages on either side have already redrawn it in their own way.',
      cast: [P('Mara Okonjo', 'Della Fenn'), P('Tomasz Reyes', 'Ilario'), P('Hanne Vos', 'Magistrate Lund'), P('Kit Aldana', 'Bo'), P('Rafe Sorensen', 'The Surveyor')],
      palette: ['#16324a', '#e9dcc4', '#d9a441', '#f6efe2'], motif: 'sun', type_face: 'serif',
      resolution: '4K', hdr: 'Dolby Vision', audio: 'Atmos', added: 2, progress: 0.42, score: 91,
    },
    {
      id: 'low-orbit', title: 'Low Orbit Lullaby', type: 'movie', year: 2023, rating: 'PG', runtime: 104,
      genres: ['Science Fiction', 'Drama'], director: 'Junie Park', studio: 'Halcyon Hall',
      tagline: 'Four hundred kilometers up, someone still has to sing them to sleep.',
      synopsis: 'On a crowded orbital nursery, a night-shift caretaker and an aging station AI improvise bedtime for forty children while a solar storm cuts them off from the ground.',
      cast: [P('Ada Whitlock', 'Noor'), P('Sefa Mensah', 'VESPER (voice)'), P('Lio Carvalho', 'Commander Ruiz'), P('Pip Hollander', 'Theo')],
      palette: ['#0d1430', '#3a3f8f', '#f4b6c2', '#e8ecff'], motif: 'orbit', type_face: 'sans',
      resolution: '4K', hdr: 'HDR10', audio: 'Atmos', added: 5, progress: 0.78, score: 87,
    },
    {
      id: 'kestrel-point', title: 'Kestrel Point', type: 'show', year: 2022, rating: 'TV-MA', runtime: 52,
      genres: ['Thriller', 'Crime'], director: 'Created by Oren Blaise', studio: 'North Fen Television',
      tagline: 'The lighthouse has been dark for eleven years. Last night it blinked.',
      synopsis: 'A disgraced coast-guard investigator returns to the island she left as a teenager when a signal from a decommissioned lighthouse lines up with three unsolved disappearances.',
      cast: [P('Rhiannon Teale', 'DI Cass Morrow'), P('Bastian Kohl', 'Elias Greer'), P('Yusra Haddad', 'Dr. Mina Shah'), P('Callum Ibe', 'Joel Morrow'), P('Greta Lindqvist', 'Harbourmaster Ainsley')],
      palette: ['#1d2b2a', '#56706a', '#f2c14e', '#dfe8e3'], motif: 'beacon', type_face: 'condensed',
      resolution: '4K', hdr: 'HDR10', audio: '5.1', added: 1, score: 89,
      seasons: [
        { n: 1, eps: ['The Dark Light', 'Low Water', 'Gannet Rock', 'What the Tide Keeps', 'Ainsley', 'Signal Fires'] },
        { n: 2, eps: ['Return Ferry', 'The Keeper’s Log', 'Spring Tide', 'Fog Index', 'Holloway', 'Blackout', 'The Long Swim', 'First Light'] },
      ],
      resume: { s: 2, e: 4, p: 0.31 },
    },
    {
      id: 'quiet-arithmetic', title: 'A Quiet Arithmetic', type: 'movie', year: 2021, rating: 'PG-13', runtime: 117,
      genres: ['Drama'], director: 'Solveig Arnadottir', studio: 'Eastlight',
      tagline: 'Some debts are counted in years.',
      synopsis: 'A retired bookkeeper balances the accounts of a failing village bakery and quietly discovers the owner has been feeding half the town on credit for a decade.',
      cast: [P('Henrik Dahl', 'Arne'), P('Lucía Ferro', 'Pilar'), P('Odile March', 'Signe'), P('Benedikt Rau', 'The Mayor')],
      palette: ['#e7e1d5', '#9a8f7e', '#b2452f', '#2b2621'], motif: 'grid', type_face: 'serif',
      resolution: '1080p', hdr: null, audio: '5.1', added: 40, progress: 0, score: 84, watched: true,
    },
    {
      id: 'night-ferry', title: 'Night Ferry to Vell', type: 'movie', year: 2024, rating: 'R', runtime: 109,
      genres: ['Mystery', 'Thriller'], director: 'Marek Ostrowski', studio: 'Grey Channel',
      tagline: 'Eleven passengers boarded. Twelve tickets were punched.',
      synopsis: 'On the last overnight crossing before the line closes for good, a ship’s purser tries to work out which of the passengers bought a ticket for someone who was never seen boarding.',
      cast: [P('Ilse Brandt', 'Purser Hale'), P('Dev Raman', 'Mr. Askew'), P('Corinne Labelle', 'Madame Ostend'), P('Joon Seo', 'The Engineer')],
      palette: ['#0f1b24', '#2f4a5c', '#e45b3c', '#cfe0ea'], motif: 'wave', type_face: 'condensed',
      resolution: '4K', hdr: 'Dolby Vision', audio: 'Atmos', added: 3, progress: 0.12, score: 82,
    },
    {
      id: 'copper-hollow', title: 'Copper Hollow', type: 'show', year: 2020, rating: 'TV-14', runtime: 48,
      genres: ['Western', 'Drama'], director: 'Created by Wren Talbot', studio: 'Dry Creek Studios',
      tagline: 'The mine gave the town a name. Then it took the rest.',
      synopsis: 'In a dying copper town in 1889, a widowed assayer and a travelling preacher compete for the trust of miners who know the seam is running out.',
      cast: [P('Abigail Stroud', 'Hester Coyle'), P('Marcus Delane', 'Rev. Amos Pike'), P('Tobias Wynn', 'Sheriff Dorn'), P('Nell Quarry', 'Little Ruth')],
      palette: ['#3a1f14', '#a8552b', '#e8b85b', '#f4e6cf'], motif: 'peaks', type_face: 'serif',
      resolution: '1080p', hdr: null, audio: '5.1', added: 18, score: 80,
      seasons: [
        { n: 1, eps: ['Assay', 'The Preacher Comes', 'Tailings', 'Dry Season', 'Seam'] },
        { n: 2, eps: ['Claim Jumpers', 'Hester', 'Winter Shaft', 'Ruth’s Map', 'The Last Ore Cart', 'Copper Hollow'] },
        { n: 3, eps: ['Ghost Town', 'Railhead', 'Pike’s Sermon', 'Smelter'] },
      ],
      resume: { s: 3, e: 2, p: 0.64 },
    },
    {
      id: 'glasshouse-year', title: 'The Glasshouse Year', type: 'movie', year: 2022, rating: 'G', runtime: 94,
      genres: ['Documentary', 'Nature'], director: 'Priya Natarajan', studio: 'Fieldnote Films',
      tagline: 'One greenhouse. Fifty-two weeks. Nobody in charge.',
      synopsis: 'A time-lapse year inside a Victorian botanical greenhouse after its last gardener retires, as orchids, moths, and a family of wrens take over.',
      cast: [P('Priya Natarajan', 'Narrator'), P('Gwen Hollis', 'Herself'), P('Ade Bamgbose', 'Himself')],
      palette: ['#0f2a1f', '#3d7a52', '#f2d16b', '#e3f2e4'], motif: 'bloom', type_face: 'serif',
      resolution: '4K', hdr: 'HDR10', audio: 'Stereo', added: 9, progress: 0, score: 93,
    },
    {
      id: 'sundial', title: 'Sundial', type: 'movie', year: 2025, rating: 'PG-13', runtime: 131,
      genres: ['Science Fiction', 'Adventure'], director: 'Teodor Vasile', studio: 'Halcyon Hall',
      tagline: 'At noon on Kepler Ridge, the shadows point backward.',
      synopsis: 'A survey crew on a tidally locked moon follows a shadow that moves against the light and uncovers a calendar built by something that measured time in reverse.',
      cast: [P('Nia Castellanos', 'Dr. Ione Reyes'), P('Gideon Marsh', 'Pilot Haskell'), P('Amaru Quispe', 'Tomas'), P('Freya Lund', 'Commander Ostrava')],
      palette: ['#2a0f05', '#c2531b', '#ffd27a', '#fff0d9'], motif: 'dial', type_face: 'condensed',
      resolution: '4K', hdr: 'Dolby Vision', audio: 'Atmos', added: 0, progress: 0, score: 88,
    },
    {
      id: 'marrow-moss', title: 'Marrow & Moss', type: 'movie', year: 2023, rating: 'R', runtime: 99,
      genres: ['Horror'], director: 'Agnes Rook', studio: 'Underbrush',
      tagline: 'The forest remembers what you buried.',
      synopsis: 'Three siblings inherit a woodland cottage and learn why their grandmother kept the moss off the north wall, every single morning, for sixty years.',
      cast: [P('Elsie Varga', 'June'), P('Cormac Tully', 'Owen'), P('Priya Bose', 'Ada'), P('Hollis Grey', 'Mr. Penhallow')],
      palette: ['#0c140f', '#2e4430', '#b9d27a', '#d9e6d0'], motif: 'roots', type_face: 'serif',
      resolution: '1080p', hdr: 'HDR10', audio: '5.1', added: 22, progress: 0.55, score: 76,
    },
    {
      id: 'second-tenor', title: 'Second Tenor', type: 'movie', year: 2022, rating: 'PG-13', runtime: 101,
      genres: ['Comedy', 'Music'], director: 'Bea Oyelaran', studio: 'Brightside',
      tagline: 'He was never the lead. That was the point.',
      synopsis: 'When a regional choir’s star soloist loses his voice before the national finals, the most forgettable second tenor in the section has nine days to become unforgettable.',
      cast: [P('Felix Amari', 'Gareth Pugh'), P('Joanna Price', 'Director Wynne'), P('Samir Haddad', 'Rhys'), P('Lottie Evans', 'Mam')],
      palette: ['#f3e6f5', '#7b3f8c', '#ffb703', '#2a1430'], motif: 'stripes', type_face: 'sans',
      resolution: '1080p', hdr: null, audio: 'Stereo', added: 31, progress: 0, score: 81, watched: true,
    },
    {
      id: 'atlas-small-rooms', title: 'Atlas of Small Rooms', type: 'show', year: 2024, rating: 'TV-PG', runtime: 28,
      genres: ['Documentary'], director: 'Created by Kai Holm', studio: 'Fieldnote Films',
      tagline: 'The world, one doorway at a time.',
      synopsis: 'Each episode visits a single room somewhere in the world, a tailor’s back office, a Lisbon tram depot, a lighthouse kitchen, and listens to the people who keep it running.',
      cast: [P('Kai Holm', 'Host')],
      palette: ['#efe9df', '#c7b8a1', '#3f6e8c', '#1f2a33'], motif: 'door', type_face: 'sans',
      resolution: '4K', hdr: null, audio: 'Stereo', added: 6, score: 90,
      seasons: [{ n: 1, eps: ['The Tailor', 'Tram Depot', 'Lighthouse Kitchen', 'Night Pharmacy', 'Bell Foundry', 'The Projection Booth', 'Ferry Galley', 'Seed Bank'] }],
      resume: { s: 1, e: 6, p: 0.12 },
    },
    {
      id: 'velvet-interval', title: 'Velvet Interval', type: 'movie', year: 2019, rating: 'PG-13', runtime: 112,
      genres: ['Romance', 'Drama'], director: 'Laurent Achebe', studio: 'Maison Clair',
      tagline: 'Fifteen minutes between acts. Thirty years between them.',
      synopsis: 'An usher and a cellist meet in the same theatre during the same intermission, once a decade, for thirty years.',
      cast: [P('Camille Osei', 'Juliette'), P('Anton Varga', 'Paul'), P('Rosa Ilić', 'Madame Fournier')],
      palette: ['#2b0a14', '#7d1d3f', '#f0b37e', '#f6dfe6'], motif: 'curtain', type_face: 'serif',
      resolution: '1080p', hdr: null, audio: '5.1', added: 60, progress: 0, score: 85,
    },
    {
      id: 'fieldwork', title: 'Fieldwork', type: 'show', year: 2021, rating: 'TV-14', runtime: 24,
      genres: ['Comedy'], director: 'Created by Dana Mbeki', studio: 'Brightside',
      tagline: 'Science is hard. Sharing a tent is harder.',
      synopsis: 'Four underfunded graduate researchers spend a summer counting beetles in a meadow that a developer wants to pave by September.',
      cast: [P('Ola Ferreira', 'Bea'), P('Theo Lam', 'Marcus'), P('Ruth Cole', 'Prof. Hendry'), P('Ziggy Adeyemi', 'Kwame')],
      palette: ['#e6f0c8', '#7aa33f', '#e94f37', '#1f2b12'], motif: 'meadow', type_face: 'sans',
      resolution: '1080p', hdr: null, audio: 'Stereo', added: 14, score: 83,
      seasons: [
        { n: 1, eps: ['Pitfall Traps', 'Grant Season', 'The Developer', 'Night Survey', 'Beetle 47', 'Townhall'] },
        { n: 2, eps: ['Second Summer', 'Peer Review', 'Rain Delay', 'The Bulldozer', 'Defence'] },
      ],
      resume: { s: 2, e: 1, p: 0 },
    },
    {
      id: 'ninth-harbor', title: 'Ninth Harbor', type: 'movie', year: 2024, rating: 'R', runtime: 122,
      genres: ['Action', 'Thriller'], director: 'Yuki Tanabe', studio: 'Grey Channel',
      tagline: 'Eight harbors closed. One still answers the radio.',
      synopsis: 'A tugboat captain with a stolen manifest has one night to bring a drifting container ship into the only port still willing to take it.',
      cast: [P('Ray Okafor', 'Captain Dunn'), P('Mei Lin Zhao', 'Harbour Pilot Ko'), P('Ezra Stein', 'Varga'), P('Tilde Aune', 'Signe')],
      palette: ['#0a0f14', '#1e3a52', '#ff6b35', '#dbe7f0'], motif: 'rails', type_face: 'condensed',
      resolution: '4K', hdr: 'Dolby Vision', audio: 'Atmos', added: 4, progress: 0, score: 79,
    },
    {
      id: 'long-thaw', title: 'The Long Thaw', type: 'movie', year: 2020, rating: 'PG', runtime: 88,
      genres: ['Documentary', 'Nature'], director: 'Ansel Kvist', studio: 'Fieldnote Films',
      tagline: 'Listen closely. The ice is talking.',
      synopsis: 'Glaciologists and Sámi reindeer herders spend a winter on the same ice shelf, recording the sounds it makes as it slowly lets go.',
      cast: [P('Ansel Kvist', 'Narrator'), P('Inga Saari', 'Herself')],
      palette: ['#dfeef5', '#8fb7c9', '#1d4e6b', '#0e2230'], motif: 'ice', type_face: 'sans',
      resolution: '4K', hdr: 'HDR10', audio: '5.1', added: 70, progress: 0, score: 88, watched: true,
    },
    {
      id: 'static-bloom', title: 'Static Bloom', type: 'show', year: 2025, rating: 'TV-MA', runtime: 55,
      genres: ['Science Fiction', 'Mystery'], director: 'Created by Ama Kusi', studio: 'Halcyon Hall',
      tagline: 'The flowers started broadcasting on a Tuesday.',
      synopsis: 'When a field of genetically modified poppies begins emitting a radio signal, a burned-out signals engineer and a botanist race to decode it before the military does.',
      cast: [P('Zara Mensah', 'Iyla Brooks'), P('Lukas Fehr', 'Dr. Paolo Ruiz'), P('Hye-jin Park', 'Colonel Ahn'), P('Owen Marr', 'Simeon')],
      palette: ['#150a24', '#5b2a86', '#ff7eb6', '#f0e6ff'], motif: 'signal', type_face: 'condensed',
      resolution: '4K', hdr: 'Dolby Vision', audio: 'Atmos', added: 1, score: 86,
      seasons: [{ n: 1, eps: ['Carrier Wave', 'Pollen Count', 'The Antenna Field', 'Interference', 'Bloom', 'Dead Air', 'Harmonics'] }],
      resume: { s: 1, e: 3, p: 0.48 },
    },
    {
      id: 'gull-island-radio', title: 'Gull Island Radio', type: 'movie', year: 2021, rating: 'PG', runtime: 96,
      genres: ['Comedy', 'Family'], director: 'Maeve Doherty', studio: 'Brightside',
      tagline: 'Population 212. Listeners: about 9.',
      synopsis: 'A teenager on a tiny island takes over the community radio station for the summer and accidentally becomes the most important voice in a fishing dispute.',
      cast: [P('Niamh Burke', 'Orla'), P('Pádraig Ó Sé', 'Granda Con'), P('Ife Adebayo', 'Tunde'), P('Siobhán Hart', 'Mrs. Kearney')],
      palette: ['#d7f0f2', '#4fa3b0', '#ffcf56', '#173a40'], motif: 'island', type_face: 'sans',
      resolution: '1080p', hdr: null, audio: 'Stereo', added: 26, progress: 0, score: 84,
    },
    {
      id: 'orchard-ash', title: 'Orchard Under Ash', type: 'movie', year: 2024, rating: 'PG-13', runtime: 119,
      genres: ['Drama'], director: 'Lena Moravec', studio: 'Eastlight',
      tagline: 'The fire took the trees. Not the roots.',
      synopsis: 'After a wildfire burns through her family’s apple orchard, a woman who swore she would never farm again has one season to decide whether to replant.',
      cast: [P('Klara Novak', 'Ruth'), P('Daniel Achterberg', 'Sam'), P('Ines Okoye', 'Grandmother Ivy')],
      palette: ['#1b1513', '#6b4a3a', '#e3874a', '#efe4d8'], motif: 'trees', type_face: 'serif',
      resolution: '4K', hdr: 'HDR10', audio: '5.1', added: 8, progress: 0.9, score: 89,
    },
    {
      id: 'hinterland-express', title: 'Hinterland Express', type: 'movie', year: 2023, rating: 'PG-13', runtime: 114,
      genres: ['Action', 'Adventure'], director: 'Sergio Bastos', studio: 'Dry Creek Studios',
      tagline: 'No stops. No brakes. No signal.',
      synopsis: 'A freight train with a failed brake line barrels across eight hundred kilometres of empty plains while the one passenger aboard learns to drive it.',
      cast: [P('Iker Domínguez', 'Rafa'), P('Tessa Holm', 'Controller Ames'), P('Kwame Asante', 'Gil')],
      palette: ['#f1e3c6', '#c97b2a', '#1d3557', '#2b1a0e'], motif: 'rails', type_face: 'condensed',
      resolution: '4K', hdr: 'HDR10', audio: 'Atmos', added: 12, progress: 0, score: 77,
    },
    {
      id: 'little-cartographers', title: 'Little Cartographers', type: 'movie', year: 2022, rating: 'G', runtime: 86,
      genres: ['Animation', 'Family'], director: 'Hana Mori', studio: 'Paper Fox Animation',
      tagline: 'If it’s not on the map, draw it.',
      synopsis: 'Two siblings discover that the hand-drawn maps in their grandfather’s attic change the neighbourhood whenever they add a new street.',
      cast: [P('Sora Kim', 'Maya (voice)'), P('Leo Fontaine', 'Jun (voice)'), P('Bernard Okri', 'Grandpa Ode (voice)')],
      palette: ['#fff2d6', '#f28c6a', '#2a9d8f', '#264653'], motif: 'map', type_face: 'sans',
      resolution: '4K', hdr: null, audio: '5.1', added: 33, progress: 0, score: 92,
    },
    {
      id: 'blue-hour-club', title: 'Blue Hour Club', type: 'movie', year: 2023, rating: 'PG-13', runtime: 97,
      genres: ['Documentary', 'Music'], director: 'Ronan Achebe', studio: 'Late Set',
      tagline: 'Forty years of jazz in a room built for sixty.',
      synopsis: 'The last night of a tiny basement jazz club, told through the musicians who played their first gigs there and came back for its last.',
      cast: [P('Ronan Achebe', 'Narrator'), P('Della Shore', 'Herself'), P('Ike Mbatha', 'Himself')],
      palette: ['#0b1440', '#2546a8', '#7fd1ff', '#e6f0ff'], motif: 'moon', type_face: 'serif',
      resolution: '1080p', hdr: null, audio: '5.1', added: 15, progress: 0.25, score: 90,
    },
    {
      id: 'undertow-ledger', title: 'The Undertow Ledger', type: 'show', year: 2023, rating: 'TV-MA', runtime: 58,
      genres: ['Crime', 'Drama'], director: 'Created by Faisal Rahim', studio: 'North Fen Television',
      tagline: 'Every number in it is a name.',
      synopsis: 'A forensic accountant finds a hand-kept ledger inside a sunken trawler and traces its entries through three generations of a port city’s quietest crime family.',
      cast: [P('Samira Qureshi', 'Leila Haddad'), P('Owen Price', 'Det. Cal Morgan'), P('Ida Rasmussen', 'Margit Vos'), P('Theo Adair', 'Little Teddy')],
      palette: ['#0e1414', '#2d4b4b', '#c9a227', '#e0e8e4'], motif: 'ledger', type_face: 'serif',
      resolution: '4K', hdr: 'HDR10', audio: '5.1', added: 45, score: 87,
      seasons: [
        { n: 1, eps: ['Salvage', 'Column B', 'Margit', 'Carried Forward', 'Debit', 'The Auditor'] },
        { n: 2, eps: ['Reconciliation', 'Teddy', 'Bad Debt', 'Write-Off', 'Balance'] },
      ],
      resume: { s: 1, e: 5, p: 0.7 },
    },
    {
      id: 'wintermint', title: 'Wintermint', type: 'movie', year: 2024, rating: 'G', runtime: 82,
      genres: ['Animation', 'Family'], director: 'Oskar Lindgren', studio: 'Paper Fox Animation',
      tagline: 'The coldest candy shop in the north just lost its cold.',
      synopsis: 'A young confectioner and a grumpy snow hare travel to the top of the world to bring winter back to their peppermint village.',
      cast: [P('Elin Berg', 'Tove (voice)'), P('Mo Farouk', 'Hare (voice)'), P('Astrid Nyman', 'Mormor (voice)')],
      palette: ['#e3f6f1', '#7cc6b6', '#e63946', '#1b3b36'], motif: 'snow', type_face: 'sans',
      resolution: '4K', hdr: 'HDR10', audio: '5.1', added: 7, progress: 0, score: 85,
    },
  ];

  // Expand episodes with deterministic runtimes, synopses and watched/progress.
  const EP_LINES = [
    'An old acquaintance turns up with a question nobody wants to answer.',
    'A routine errand goes sideways and two allies stop speaking.',
    'The truth about the first night begins to surface.',
    'A storm forces everyone under one roof.',
    'Someone keeps a promise they should have broken.',
    'A missing page changes what everyone thought they knew.',
    'Loyalties shift as the deadline arrives.',
    'The finale brings everyone back to where it started.',
  ];
  L.forEach(item => {
    item.score = item.score || 80;
    if (item.type !== 'show') return;
    const r = rng(hash(item.id + 'eps'));
    item.seasons.forEach(season => {
      season.episodes = season.eps.map((title, i) => {
        const ep = {
          s: season.n, e: i + 1, title,
          runtime: Math.round(item.runtime + (r() - 0.5) * 12),
          synopsis: EP_LINES[(i + season.n * 3) % EP_LINES.length],
          progress: 0, watched: false,
        };
        const rs = item.resume;
        if (rs && (season.n < rs.s || (season.n === rs.s && ep.e < rs.e))) ep.watched = true;
        if (rs && season.n === rs.s && ep.e === rs.e) ep.progress = rs.p;
        return ep;
      });
      delete season.eps;
    });
    item.episodeCount = item.seasons.reduce((n, s) => n + s.episodes.length, 0);
  });

  // ---------- procedural artwork ----------
  const esc = s => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  const FONTS = {
    serif: "Georgia, 'Times New Roman', serif",
    sans: "'Helvetica Neue', Helvetica, Arial, sans-serif",
    condensed: "'Arial Narrow', 'Helvetica Neue', Arial, sans-serif",
  };

  function motif(item, r, W, H, mode) {
    const [bg, mid, acc, lt] = item.palette;
    const out = [];
    const cx = W / 2;
    switch (item.motif) {
      case 'sun': {
        const hy = H * 0.62;
        out.push(`<circle cx="${cx + (r() - 0.5) * W * 0.2}" cy="${hy - H * 0.05}" r="${Math.min(W, H) * 0.24}" fill="${acc}"/>`);
        out.push(`<rect x="0" y="${hy}" width="${W}" height="${H - hy}" fill="${mid}"/>`);
        for (let i = 0; i < 9; i++) {
          const y = hy + 10 + i * i * 6;
          out.push(`<rect x="${W * 0.1 + r() * W * 0.2}" y="${y}" width="${W * (0.3 + r() * 0.4)}" height="${2 + i * 0.6}" fill="${bg}" opacity="${0.15 + i * 0.05}"/>`);
        }
        break;
      }
      case 'orbit': {
        for (let i = 0; i < 70; i++) out.push(`<circle cx="${r() * W}" cy="${r() * H}" r="${r() * 1.6 + 0.3}" fill="${lt}" opacity="${0.3 + r() * 0.7}"/>`);
        out.push(`<circle cx="${cx}" cy="${H * 1.15}" r="${W * 0.75}" fill="${mid}"/>`);
        for (let i = 0; i < 4; i++) out.push(`<ellipse cx="${cx}" cy="${H * 0.45}" rx="${W * (0.18 + i * 0.1)}" ry="${W * (0.05 + i * 0.03)}" fill="none" stroke="${acc}" stroke-width="1.5" opacity="${0.9 - i * 0.18}" transform="rotate(-14 ${cx} ${H * 0.45})"/>`);
        out.push(`<circle cx="${cx}" cy="${H * 0.45}" r="${W * 0.06}" fill="${lt}"/>`);
        break;
      }
      case 'beacon': {
        out.push(`<polygon points="${cx},${H * 0.35} ${-W * 0.4},${H * 0.15} ${-W * 0.4},${H * 0.45}" fill="${acc}" opacity="0.35"/>`);
        out.push(`<polygon points="${cx},${H * 0.35} ${W * 1.4},${H * 0.2} ${W * 1.4},${H * 0.42}" fill="${acc}" opacity="0.18"/>`);
        out.push(`<path d="M${cx - W * 0.05} ${H * 0.36} L${cx - W * 0.08} ${H * 0.75} L${cx + W * 0.08} ${H * 0.75} L${cx + W * 0.05} ${H * 0.36} Z" fill="${lt}"/>`);
        for (let i = 0; i < 3; i++) out.push(`<rect x="${cx - W * 0.07}" y="${H * (0.45 + i * 0.1)}" width="${W * 0.14}" height="${H * 0.03}" fill="${bg}" opacity="0.7"/>`);
        out.push(`<circle cx="${cx}" cy="${H * 0.34}" r="${W * 0.035}" fill="${acc}"/>`);
        out.push(`<path d="M0 ${H * 0.78} Q ${W * 0.3} ${H * 0.72} ${cx} ${H * 0.75} T ${W} ${H * 0.76} V ${H} H 0 Z" fill="${mid}"/>`);
        break;
      }
      case 'grid': {
        const n = 7, s = W / n;
        for (let x = 0; x < n; x++) for (let y = 0; y < Math.ceil(H / s); y++) {
          const v = r();
          if (v > 0.82) out.push(`<rect x="${x * s + 3}" y="${y * s + 3}" width="${s - 6}" height="${s - 6}" fill="${acc}" opacity="${0.6 + r() * 0.4}"/>`);
          else if (v > 0.55) out.push(`<rect x="${x * s + 3}" y="${y * s + 3}" width="${s - 6}" height="${s - 6}" fill="${mid}" opacity="0.35"/>`);
        }
        for (let x = 0; x <= n; x++) out.push(`<line x1="${x * s}" y1="0" x2="${x * s}" y2="${H}" stroke="${lt}" stroke-width="1" opacity="0.3"/>`);
        break;
      }
      case 'wave': {
        for (let i = 0; i < 12; i++) {
          const y = H * 0.4 + i * H * 0.05;
          const a = 10 + i * 3;
          out.push(`<path d="M0 ${y} Q ${W * 0.25} ${y - a} ${W * 0.5} ${y} T ${W} ${y} V ${H} H 0 Z" fill="${i % 2 ? mid : bg}" opacity="${0.5 + i * 0.04}"/>`);
        }
        out.push(`<circle cx="${W * 0.72}" cy="${H * 0.22}" r="${W * 0.08}" fill="${lt}" opacity="0.9"/>`);
        out.push(`<rect x="${W * 0.2}" y="${H * 0.36}" width="${W * 0.32}" height="${H * 0.04}" fill="${acc}"/>`);
        out.push(`<rect x="${W * 0.26}" y="${H * 0.31}" width="${W * 0.14}" height="${H * 0.05}" fill="${acc}"/>`);
        break;
      }
      case 'peaks': {
        for (let l = 0; l < 4; l++) {
          let d = `M0 ${H}`;
          const base = H * (0.45 + l * 0.12);
          for (let x = 0; x <= W; x += W / 6) d += ` L${x} ${base - r() * H * 0.18}`;
          d += ` L${W} ${H} Z`;
          out.push(`<path d="${d}" fill="${[mid, acc, mid, bg][l]}" opacity="${[0.6, 0.85, 0.9, 1][l]}"/>`);
        }
        out.push(`<circle cx="${W * 0.7}" cy="${H * 0.25}" r="${W * 0.1}" fill="${lt}" opacity="0.85"/>`);
        break;
      }
      case 'bloom': {
        const cy = H * 0.42;
        for (let i = 0; i < 12; i++) {
          const a = (i / 12) * 360;
          out.push(`<ellipse cx="${cx}" cy="${cy - W * 0.16}" rx="${W * 0.07}" ry="${W * 0.17}" fill="${i % 2 ? mid : acc}" opacity="0.85" transform="rotate(${a} ${cx} ${cy})"/>`);
        }
        out.push(`<circle cx="${cx}" cy="${cy}" r="${W * 0.07}" fill="${lt}"/>`);
        for (let i = 0; i < 9; i++) out.push(`<line x1="${(i + 0.5) * W / 9}" y1="${H}" x2="${(i + 0.5) * W / 9 + (r() - 0.5) * 40}" y2="${H * (0.7 + r() * 0.15)}" stroke="${mid}" stroke-width="4"/>`);
        break;
      }
      case 'dial': {
        const cy = H * 0.45, R = W * 0.36;
        out.push(`<circle cx="${cx}" cy="${cy}" r="${R}" fill="${mid}" opacity="0.6"/>`);
        for (let i = 0; i < 24; i++) {
          const a = (i / 24) * Math.PI * 2;
          out.push(`<line x1="${cx + Math.cos(a) * R * 0.8}" y1="${cy + Math.sin(a) * R * 0.8}" x2="${cx + Math.cos(a) * R}" y2="${cy + Math.sin(a) * R}" stroke="${lt}" stroke-width="${i % 6 ? 1.5 : 4}"/>`);
        }
        out.push(`<polygon points="${cx},${cy} ${cx + R * 1.6},${cy + R * 1.3} ${cx + R * 1.2},${cy + R * 1.6}" fill="${bg}" opacity="0.8"/>`);
        out.push(`<polygon points="${cx - 6},${cy} ${cx},${cy - R * 0.9} ${cx + 6},${cy}" fill="${acc}"/>`);
        out.push(`<rect x="0" y="${H * 0.82}" width="${W}" height="${H * 0.18}" fill="${bg}"/>`);
        break;
      }
      case 'roots': {
        for (let i = 0; i < 26; i++) {
          let x = r() * W, y = H * 0.15, d = `M${x} ${y}`;
          for (let s = 0; s < 6; s++) { x += (r() - 0.5) * 60; y += H * 0.13; d += ` L${x} ${y}`; }
          out.push(`<path d="${d}" fill="none" stroke="${i % 5 ? mid : acc}" stroke-width="${1 + r() * 4}" opacity="${0.4 + r() * 0.6}"/>`);
        }
        out.push(`<rect x="0" y="0" width="${W}" height="${H * 0.18}" fill="${bg}"/>`);
        out.push(`<rect x="${cx - W * 0.12}" y="${H * 0.05}" width="${W * 0.24}" height="${H * 0.13}" fill="${mid}"/>`);
        out.push(`<polygon points="${cx - W * 0.16},${H * 0.07} ${cx},${H * 0.0} ${cx + W * 0.16},${H * 0.07}" fill="${mid}"/>`);
        out.push(`<rect x="${cx - W * 0.03}" y="${H * 0.1}" width="${W * 0.06}" height="${W * 0.06}" fill="${acc}"/>`);
        break;
      }
      case 'stripes': {
        for (let i = 0; i < 9; i++) out.push(`<rect x="${i * W / 9}" y="0" width="${W / 18}" height="${H}" fill="${mid}" opacity="${0.15 + (i % 3) * 0.1}"/>`);
        for (let i = 0; i < 5; i++) {
          const x = W * (0.18 + i * 0.16), h = H * (0.2 + r() * 0.3);
          out.push(`<ellipse cx="${x}" cy="${H * 0.78 - h}" rx="${W * 0.05}" ry="${W * 0.06}" fill="${i === 2 ? acc : mid}"/>`);
          out.push(`<rect x="${x - W * 0.06}" y="${H * 0.78 - h + W * 0.06}" width="${W * 0.12}" height="${h}" rx="${W * 0.04}" fill="${i === 2 ? acc : mid}"/>`);
        }
        break;
      }
      case 'door': {
        const dw = W * 0.34, dh = H * 0.5, dx = cx - dw / 2, dy = H * 0.2;
        out.push(`<rect x="0" y="${H * 0.72}" width="${W}" height="${H * 0.28}" fill="${mid}"/>`);
        out.push(`<path d="M${dx} ${dy + dh} V ${dy + dw / 2} A ${dw / 2} ${dw / 2} 0 0 1 ${dx + dw} ${dy + dw / 2} V ${dy + dh} Z" fill="${acc}"/>`);
        out.push(`<path d="M${dx + 12} ${dy + dh} V ${dy + dw / 2} A ${dw / 2 - 12} ${dw / 2 - 12} 0 0 1 ${dx + dw - 12} ${dy + dw / 2} V ${dy + dh} Z" fill="${lt}" opacity="0.9"/>`);
        out.push(`<polygon points="${dx + 12},${dy + dh} ${dx + dw - 12},${dy + dh} ${dx + dw + W * 0.2},${H} ${dx - W * 0.2},${H}" fill="${lt}" opacity="0.35"/>`);
        break;
      }
      case 'curtain': {
        for (let i = 0; i < 14; i++) {
          const x = i * W / 14;
          out.push(`<path d="M${x} 0 Q ${x + W / 28} ${H * 0.5} ${x} ${H} H ${x + W / 14} Q ${x + W / 28} ${H * 0.5} ${x + W / 14} 0 Z" fill="${i % 2 ? mid : bg}"/>`);
        }
        out.push(`<ellipse cx="${cx}" cy="${H * 0.55}" rx="${W * 0.25}" ry="${H * 0.35}" fill="${acc}" opacity="0.35"/>`);
        out.push(`<path d="M0 0 H ${W} V ${H * 0.12} Q ${cx} ${H * 0.2} 0 ${H * 0.12} Z" fill="${bg}"/>`);
        break;
      }
      case 'meadow': {
        out.push(`<circle cx="${W * 0.25}" cy="${H * 0.25}" r="${W * 0.12}" fill="${acc}"/>`);
        for (let i = 0; i < 90; i++) {
          const x = r() * W, y = H * (0.5 + r() * 0.5);
          out.push(`<line x1="${x}" y1="${H}" x2="${x + (r() - 0.5) * 20}" y2="${y}" stroke="${i % 7 ? mid : bg}" stroke-width="${1 + r() * 2}"/>`);
        }
        for (let i = 0; i < 8; i++) out.push(`<circle cx="${r() * W}" cy="${H * (0.55 + r() * 0.3)}" r="${3 + r() * 4}" fill="${acc}"/>`);
        break;
      }
      case 'rails': {
        const vy = H * 0.42;
        out.push(`<rect x="0" y="${vy}" width="${W}" height="${H - vy}" fill="${mid}" opacity="0.5"/>`);
        out.push(`<polygon points="${cx - 4},${vy} ${cx + 4},${vy} ${W * 0.9},${H} ${W * 0.1},${H}" fill="${bg}" opacity="0.6"/>`);
        out.push(`<line x1="${cx - 2}" y1="${vy}" x2="${W * 0.25}" y2="${H}" stroke="${lt}" stroke-width="3"/>`);
        out.push(`<line x1="${cx + 2}" y1="${vy}" x2="${W * 0.75}" y2="${H}" stroke="${lt}" stroke-width="3"/>`);
        for (let i = 1; i < 12; i++) {
          const t = Math.pow(i / 12, 2), y = vy + (H - vy) * t, half = (W * 0.32) * t + 3;
          out.push(`<line x1="${cx - half}" y1="${y}" x2="${cx + half}" y2="${y}" stroke="${lt}" stroke-width="${1 + t * 5}" opacity="0.6"/>`);
        }
        out.push(`<circle cx="${cx}" cy="${vy - 4}" r="${W * 0.05}" fill="${acc}"/>`);
        break;
      }
      case 'ice': {
        for (let i = 0; i < 7; i++) {
          const y = H * (0.3 + i * 0.1);
          let d = `M0 ${y}`;
          for (let x = 0; x <= W; x += W / 5) d += ` L${x} ${y + (r() - 0.5) * 40}`;
          d += ` L${W} ${H} L0 ${H} Z`;
          out.push(`<path d="${d}" fill="${i % 2 ? mid : lt}" opacity="${0.35 + i * 0.08}"/>`);
        }
        out.push(`<line x1="${W * 0.3}" y1="${H * 0.35}" x2="${W * 0.55}" y2="${H}" stroke="${acc}" stroke-width="2"/>`);
        out.push(`<line x1="${W * 0.55}" y1="${H * 0.6}" x2="${W * 0.8}" y2="${H * 0.7}" stroke="${acc}" stroke-width="1.5"/>`);
        break;
      }
      case 'signal': {
        for (let i = 0; i < 9; i++) out.push(`<circle cx="${cx}" cy="${H * 0.62}" r="${W * (0.08 + i * 0.09)}" fill="none" stroke="${acc}" stroke-width="2" opacity="${0.9 - i * 0.09}"/>`);
        for (let i = 0; i < 30; i++) {
          const x = r() * W, y = H * (0.7 + r() * 0.3);
          out.push(`<line x1="${x}" y1="${H}" x2="${x}" y2="${y}" stroke="${mid}" stroke-width="2"/><circle cx="${x}" cy="${y}" r="${4 + r() * 4}" fill="${i % 3 ? acc : lt}"/>`);
        }
        break;
      }
      case 'island': {
        out.push(`<rect x="0" y="${H * 0.55}" width="${W}" height="${H * 0.45}" fill="${mid}"/>`);
        out.push(`<ellipse cx="${cx}" cy="${H * 0.57}" rx="${W * 0.32}" ry="${H * 0.06}" fill="${lt}"/>`);
        out.push(`<rect x="${cx + W * 0.06}" y="${H * 0.28}" width="4" height="${H * 0.26}" fill="${lt}"/>`);
        for (let i = 0; i < 3; i++) out.push(`<path d="M${cx + W * 0.06 + 2} ${H * 0.3} m -${20 + i * 18} -${10 + i * 12} a ${20 + i * 18} ${20 + i * 18} 0 0 1 ${2 * (20 + i * 18)} 0" fill="none" stroke="${acc}" stroke-width="3" opacity="${1 - i * 0.25}"/>`);
        for (let i = 0; i < 5; i++) { const x = r() * W, y = H * (0.1 + r() * 0.3); out.push(`<path d="M${x} ${y} q 8 -8 16 0 q 8 -8 16 0" fill="none" stroke="${lt}" stroke-width="2"/>`); }
        break;
      }
      case 'trees': {
        for (let i = 0; i < 9; i++) {
          const x = (i + 0.5) * W / 9 + (r() - 0.5) * 10, top = H * (0.45 + r() * 0.15);
          out.push(`<line x1="${x}" y1="${H * 0.85}" x2="${x}" y2="${top}" stroke="${bg}" stroke-width="5"/>`);
          out.push(`<line x1="${x}" y1="${top + 30}" x2="${x - 18}" y2="${top + 5}" stroke="${bg}" stroke-width="3"/><line x1="${x}" y1="${top + 45}" x2="${x + 18}" y2="${top + 20}" stroke="${bg}" stroke-width="3"/>`);
        }
        out.push(`<rect x="0" y="${H * 0.85}" width="${W}" height="${H * 0.15}" fill="${bg}"/>`);
        for (let i = 0; i < 40; i++) out.push(`<circle cx="${r() * W}" cy="${r() * H * 0.7}" r="${r() * 2 + 0.5}" fill="${acc}" opacity="${r()}"/>`);
        out.push(`<circle cx="${W * 0.5}" cy="${H * 0.95}" r="${W * 0.03}" fill="${acc}"/>`);
        break;
      }
      case 'map': {
        for (let i = 0; i < 6; i++) {
          let d = `M${r() * W} 0`;
          for (let s = 1; s <= 5; s++) d += ` L${r() * W} ${s * H / 5}`;
          out.push(`<path d="${d}" fill="none" stroke="${i % 2 ? acc : mid}" stroke-width="${3 + r() * 4}" stroke-linecap="round" stroke-linejoin="round" opacity="0.8"/>`);
        }
        for (let i = 0; i < 6; i++) out.push(`<rect x="${r() * W * 0.85}" y="${r() * H * 0.8}" width="${W * 0.1}" height="${W * 0.1}" rx="4" fill="${lt}" stroke="${mid}" stroke-width="2"/>`);
        out.push(`<path d="M${W * 0.72} ${H * 0.2} l 10 -30 l 10 30 z" fill="${mid}"/>`);
        break;
      }
      case 'moon': {
        out.push(`<circle cx="${W * 0.62}" cy="${H * 0.28}" r="${W * 0.16}" fill="${acc}"/>`);
        out.push(`<circle cx="${W * 0.69}" cy="${H * 0.25}" r="${W * 0.15}" fill="${bg}"/>`);
        const keys = 14;
        for (let i = 0; i < keys; i++) out.push(`<rect x="${i * W / keys + 1}" y="${H * 0.72}" width="${W / keys - 2}" height="${H * 0.28}" fill="${lt}"/>`);
        for (let i = 0; i < keys; i++) if (![2, 6, 9, 13].includes(i)) out.push(`<rect x="${(i + 0.65) * W / keys}" y="${H * 0.72}" width="${W / keys * 0.7}" height="${H * 0.16}" fill="${bg}"/>`);
        for (let i = 0; i < 5; i++) out.push(`<path d="M${W * 0.1} ${H * (0.45 + i * 0.04)} Q ${cx} ${H * (0.35 + i * 0.05)} ${W * 0.9} ${H * (0.48 + i * 0.03)}" fill="none" stroke="${mid}" stroke-width="2" opacity="0.8"/>`);
        break;
      }
      case 'ledger': {
        for (let i = 0; i < 22; i++) {
          const y = H * 0.1 + i * H * 0.035;
          out.push(`<line x1="${W * 0.1}" y1="${y}" x2="${W * 0.9}" y2="${y}" stroke="${mid}" stroke-width="1"/>`);
          if (r() > 0.4) out.push(`<rect x="${W * 0.12}" y="${y - 8}" width="${W * (0.15 + r() * 0.3)}" height="4" fill="${lt}" opacity="0.4"/>`);
          if (r() > 0.6) out.push(`<rect x="${W * 0.7}" y="${y - 8}" width="${W * 0.15}" height="4" fill="${acc}" opacity="0.8"/>`);
        }
        out.push(`<line x1="${W * 0.66}" y1="${H * 0.08}" x2="${W * 0.66}" y2="${H * 0.9}" stroke="${acc}" stroke-width="2"/>`);
        out.push(`<path d="M0 ${H * 0.82} Q ${W * 0.3} ${H * 0.75} ${W * 0.6} ${H * 0.85} T ${W} ${H * 0.8} V ${H} H 0 Z" fill="${mid}" opacity="0.9"/>`);
        break;
      }
      case 'snow': {
        out.push(`<path d="M0 ${H * 0.75} Q ${W * 0.25} ${H * 0.6} ${cx} ${H * 0.7} T ${W} ${H * 0.65} V ${H} H 0 Z" fill="${lt}"/>`);
        out.push(`<path d="M0 ${H * 0.85} Q ${W * 0.3} ${H * 0.78} ${W * 0.7} ${H * 0.86} T ${W} ${H * 0.82} V ${H} H 0 Z" fill="${mid}" opacity="0.5"/>`);
        out.push(`<rect x="${cx - W * 0.1}" y="${H * 0.5}" width="${W * 0.2}" height="${H * 0.18}" fill="${acc}"/>`);
        out.push(`<polygon points="${cx - W * 0.14},${H * 0.5} ${cx},${H * 0.4} ${cx + W * 0.14},${H * 0.5}" fill="${mid}"/>`);
        for (let i = 0; i < 8; i++) out.push(`<rect x="${cx - W * 0.1 + i * W * 0.025}" y="${H * 0.5}" width="${W * 0.0125}" height="${H * 0.18}" fill="${lt}" opacity="0.7"/>`);
        for (let i = 0; i < 60; i++) out.push(`<circle cx="${r() * W}" cy="${r() * H * 0.8}" r="${r() * 3 + 1}" fill="#ffffff" opacity="${0.5 + r() * 0.5}"/>`);
        break;
      }
      default:
        out.push(`<circle cx="${cx}" cy="${H / 2}" r="${W / 3}" fill="${acc}"/>`);
    }
    return out.join('');
  }

  function wrapTitle(title, max) {
    const words = title.split(' ');
    const lines = [];
    let cur = '';
    words.forEach(w => {
      if ((cur + ' ' + w).trim().length > max && cur) { lines.push(cur); cur = w; } else cur = (cur + ' ' + w).trim();
    });
    if (cur) lines.push(cur);
    return lines;
  }

  function grain(id) {
    return `<filter id="g${id}"><feTurbulence type="fractalNoise" baseFrequency="0.9" numOctaves="2" stitchTiles="stitch"/><feColorMatrix values="0 0 0 0 0.5  0 0 0 0 0.5  0 0 0 0 0.5  0 0 0 0.09 0"/></filter>`;
  }

  const cache = new Map();
  function toURI(svg) { return 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg); }

  function posterSVG(item) {
    const W = 400, H = 600;
    const [bg, mid, acc, lt] = item.palette;
    const r = rng(hash(item.id));
    const light = luminance(bg) > 0.5;
    const ink = light ? item.palette[3] : lt;
    const face = FONTS[item.type_face];
    const isCond = item.type_face === 'condensed';
    const lines = wrapTitle(item.title.toUpperCase(), isCond ? 11 : 13);
    const size = isCond ? 54 : item.type_face === 'serif' ? 40 : 38;
    const lh = size * 1.05;
    const top = item.type_face === 'serif';
    let y0 = top ? 72 : H - 46 - (lines.length - 1) * lh;
    const text = lines.map((ln, i) => `<text x="${W / 2}" y="${y0 + i * lh}" text-anchor="middle" font-family="${face}" font-size="${size}" font-weight="${item.type_face === 'serif' ? 400 : 700}" letter-spacing="${isCond ? -1 : item.type_face === 'serif' ? 3 : 1}" fill="${ink}" ${isCond ? 'transform="scale(1 1)"' : ''}>${esc(ln)}</text>`).join('');
    const credit = `<text x="${W / 2}" y="${top ? H - 26 : 36}" text-anchor="middle" font-family="${FONTS.sans}" font-size="11" letter-spacing="3" fill="${ink}" opacity="0.75">${esc(item.director.replace('Created by ', '').toUpperCase())}</text>`;
    const shade = top
      ? `<linearGradient id="s" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${bg}" stop-opacity="0.95"/><stop offset="0.3" stop-color="${bg}" stop-opacity="0"/></linearGradient>`
      : `<linearGradient id="s" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="${bg}" stop-opacity="0.95"/><stop offset="0.35" stop-color="${bg}" stop-opacity="0"/></linearGradient>`;
    return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}"><defs><linearGradient id="b" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${bg}"/><stop offset="1" stop-color="${mix(bg, mid, 0.45)}"/></linearGradient>${shade}${grain('p')}</defs><rect width="${W}" height="${H}" fill="url(#b)"/>${motif(item, r, W, H, 'poster')}<rect width="${W}" height="${H}" fill="url(#s)"/><rect width="${W}" height="${H}" filter="url(#gp)"/>${text}${credit}</svg>`;
  }

  function backdropSVG(item, variant) {
    const W = 1600, H = 900;
    const [bg, mid] = item.palette;
    const r = rng(hash(item.id + 'bd' + (variant || 0)));
    return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" preserveAspectRatio="xMidYMid slice"><defs><linearGradient id="b" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${bg}"/><stop offset="1" stop-color="${mix(bg, mid, 0.5)}"/></linearGradient><radialGradient id="v" cx="0.5" cy="0.5" r="0.75"><stop offset="0.55" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity="0.45"/></radialGradient>${grain('b')}</defs><rect width="${W}" height="${H}" fill="url(#b)"/><g transform="${variant ? `translate(${W / 2} ${H / 2}) scale(${1.25 + r() * 0.9}) rotate(${(r() - 0.5) * 16}) translate(${-W / 2 + (r() - 0.5) * 500} ${-H / 2 + (r() - 0.5) * 260})` : `translate(${W / 2} ${H / 2}) scale(1.18) translate(${-W / 2 + 120} ${-H / 2})`}">${motif(item, r, W, H, 'backdrop')}</g><rect width="${W}" height="${H}" fill="url(#v)"/><rect width="${W}" height="${H}" filter="url(#gb)"/></svg>`;
  }

  function luminance(hex) {
    const n = parseInt(hex.slice(1), 16);
    const c = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map(v => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); });
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
  }
  function mix(a, b, t) {
    const pa = parseInt(a.slice(1), 16), pb = parseInt(b.slice(1), 16);
    const ch = s => Math.round(((pa >> s) & 255) * (1 - t) + ((pb >> s) & 255) * t);
    return '#' + [16, 8, 0].map(s => ch(s).toString(16).padStart(2, '0')).join('');
  }

  // ---------- public helpers ----------
  const byId = Object.fromEntries(L.map(i => [i.id, i]));

  function poster(item) {
    item = typeof item === 'string' ? byId[item] : item;
    const k = 'p:' + item.id;
    if (!cache.has(k)) cache.set(k, toURI(posterSVG(item)));
    return cache.get(k);
  }
  function backdrop(item, variant = 0) {
    item = typeof item === 'string' ? byId[item] : item;
    const k = 'b:' + item.id + ':' + variant;
    if (!cache.has(k)) cache.set(k, toURI(backdropSVG(item, variant)));
    return cache.get(k);
  }

  function runtime(min) {
    const h = Math.floor(min / 60), m = min % 60;
    return h ? `${h}h ${String(m).padStart(2, '0')}m` : `${m}m`;
  }
  function clock(sec) {
    sec = Math.max(0, Math.floor(sec));
    const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
    return h ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}` : `${m}:${String(s).padStart(2, '0')}`;
  }
  function addedLabel(days) {
    if (days === 0) return 'Added today';
    if (days === 1) return 'Added yesterday';
    if (days < 14) return `Added ${days} days ago`;
    if (days < 60) return `Added ${Math.round(days / 7)} weeks ago`;
    return `Added ${Math.round(days / 30)} months ago`;
  }

  /** The episode a show resumes at (or first episode). */
  function nextEpisode(item) {
    if (item.type !== 'show') return null;
    const rs = item.resume || { s: 1, e: 1 };
    const season = item.seasons.find(s => s.n === rs.s) || item.seasons[0];
    return season.episodes.find(e => e.e === rs.e) || season.episodes[0];
  }
  function episodeAfter(item, ep) {
    const season = item.seasons.find(s => s.n === ep.s);
    const idx = season.episodes.indexOf(ep);
    if (idx < season.episodes.length - 1) return season.episodes[idx + 1];
    const nextSeason = item.seasons.find(s => s.n === ep.s + 1);
    return nextSeason ? nextSeason.episodes[0] : null;
  }
  function epCode(ep) { return `S${ep.s} E${ep.e}`; }

  /** Items in progress, most recent first. Returns {item, progress, episode, remaining (min), label}. */
  function continueWatching() {
    const order = ['kestrel-point', 'salt-meridian', 'low-orbit', 'static-bloom', 'orchard-ash', 'copper-hollow', 'marrow-moss', 'undertow-ledger', 'blue-hour-club', 'night-ferry', 'atlas-small-rooms'];
    return order.map(id => byId[id]).map(item => {
      if (item.type === 'show') {
        const ep = nextEpisode(item);
        return { item, episode: ep, progress: ep.progress, remaining: Math.round(ep.runtime * (1 - ep.progress)), label: `${epCode(ep)}: ${ep.title}` };
      }
      return { item, episode: null, progress: item.progress || 0, remaining: Math.round(item.runtime * (1 - (item.progress || 0))), label: runtime(item.runtime) };
    }).filter(c => c.progress > 0 && c.progress < 0.97);
  }
  function recentlyAdded(n = 12) { return [...L].sort((a, b) => a.added - b.added).slice(0, n); }
  function genres() { return [...new Set(L.flatMap(i => i.genres))].sort(); }
  function related(item, n = 8) {
    return L.filter(i => i !== item).map(i => ({ i, s: i.genres.filter(g => item.genres.includes(g)).length * 10 + (i.studio === item.studio ? 5 : 0) + (i.score / 100) }))
      .sort((a, b) => b.s - a.s).slice(0, n).map(x => x.i);
  }

  /**
   * Filter + sort the library.
   * opts: { query, type: 'all'|'movie'|'show', genre: 'all'|<genre>, status: 'all'|'unwatched'|'in-progress'|'watched', sort: 'title'|'year'|'added'|'score'|'runtime', dir: 'asc'|'desc' }
   */
  function query(opts = {}) {
    const q = (opts.query || '').trim().toLowerCase();
    let list = L.filter(i => {
      if (opts.type && opts.type !== 'all' && i.type !== opts.type) return false;
      if (opts.genre && opts.genre !== 'all' && !i.genres.includes(opts.genre)) return false;
      if (opts.status && opts.status !== 'all' && status(i) !== opts.status) return false;
      if (q) {
        const hay = [i.title, i.director, i.studio, i.year, ...i.genres, ...i.cast.map(c => c.name)].join(' ').toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
    const sort = opts.sort || 'title';
    const key = {
      title: i => i.title.replace(/^(The|A) /, ''),
      year: i => i.year, added: i => i.added, score: i => i.score,
      runtime: i => i.type === 'show' ? i.runtime * i.episodeCount : i.runtime,
    }[sort];
    const defaultDir = { title: 'asc', year: 'desc', added: 'asc', score: 'desc', runtime: 'desc' }[sort];
    const dir = (opts.dir || defaultDir) === 'asc' ? 1 : -1;
    list.sort((a, b) => { const x = key(a), y = key(b); return (x < y ? -1 : x > y ? 1 : 0) * dir; });
    return list;
  }
  function status(i) {
    if (i.type === 'show') {
      const eps = i.seasons.flatMap(s => s.episodes);
      if (eps.every(e => e.watched)) return 'watched';
      if (eps.some(e => e.watched || e.progress > 0)) return 'in-progress';
      return 'unwatched';
    }
    if (i.watched) return 'watched';
    if (i.progress > 0) return 'in-progress';
    return 'unwatched';
  }

  const SUBTITLES = [
    'You said the tide would be out by now.',
    'It is. Look how far the water went.',
    '[distant gulls]',
    'Then why does it feel like it’s coming back?',
    'Because it always does. That’s the arrangement.',
    'Keep walking. Don’t look at the line.',
    '[wind rising]',
    'Whoever drew this map never stood here.',
    'Maybe that was the point.',
    '',
  ];
  function subtitleAt(t) { const slot = Math.floor(t / 4); return SUBTITLES[slot % SUBTITLES.length]; }

  const CHAPTERS = ['Opening', 'The arrival', 'First crossing', 'Night', 'The argument', 'Low tide', 'Return', 'Credits'];
  const CHAPTER_WEIGHTS = [0.6, 1.4, 1.1, 1.6, 0.9, 1.3, 1.0, 0.35];
  function chapters(durationSec) {
    const total = CHAPTER_WEIGHTS.reduce((a, b) => a + b, 0);
    let acc = 0;
    return CHAPTERS.map((title, i) => { const start = Math.round(durationSec * acc / total); acc += CHAPTER_WEIGHTS[i]; return { title, start }; });
  }

  window.Motion = window.Motion || {};
  Object.assign(window.Motion, {
    library: L, get: id => byId[id], poster, backdrop,
    fmt: { runtime, clock, added: addedLabel, epCode },
    continueWatching, recentlyAdded, genres, related, query, status,
    nextEpisode, episodeAfter, subtitleAt, chapters,
    user: { name: 'Rowan', server: 'Den NAS' },
  });
})();
