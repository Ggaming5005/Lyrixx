// Demo content for mock.js. Every song, artist, album, lyric line and cover
// here is made up for Lyrix; none of it is a real song.

/** Parses `[mm:ss.xx] text` lines into `{ startMs, text }`. */
function lrc(text) {
  return text
    .trim()
    .split('\n')
    .map((row) => {
      const match = /^\[(\d+):(\d+(?:\.\d+)?)\]\s?(.*)$/.exec(row.trim());
      if (!match) {
        throw new Error(`bad demo lyric line: ${row}`);
      }
      return { startMs: Math.round((Number(match[1]) * 60 + Number(match[2])) * 1000), text: match[3] };
    });
}

/** Unsynced lines spread like `Lyrics::spread_evenly`: first at 5 %, last at 90 %. */
function spread(lines, durationMs) {
  const first = durationMs * 0.05;
  const last = durationMs * 0.9;
  const step = lines.length > 1 ? (last - first) / (lines.length - 1) : 0;
  return lines.map((text, i) => ({ startMs: Math.floor(first + step * i), text }));
}

function svgUrl(svg) {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg.replace(/\s*\n\s*/g, ' ').trim())}`;
}

const STARS = [
  [64, 72, 1.6, 0.9],
  [120, 150, 1.1, 0.6],
  [210, 60, 1.3, 0.75],
  [96, 260, 1, 0.5],
  [520, 96, 1.5, 0.8],
  [560, 210, 1, 0.55],
  [180, 330, 1.2, 0.45],
  [470, 410, 1.1, 0.5],
  [36, 380, 1.4, 0.6],
  [300, 40, 1, 0.5],
  [560, 360, 1.3, 0.4],
]
  .map(([x, y, r, o]) => `<circle cx='${x}' cy='${y}' r='${r}' fill='#fff' opacity='${o}'/>`)
  .join('');

const COVERS = {
  paperSatellites: svgUrl(`
    <svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 600 600'>
      <defs>
        <linearGradient id='s' x1='0' y1='0' x2='.35' y2='1'>
          <stop offset='0' stop-color='#140f3a'/><stop offset='.55' stop-color='#4a1d72'/><stop offset='1' stop-color='#d8446e'/>
        </linearGradient>
        <radialGradient id='p' cx='.38' cy='.32' r='.75'>
          <stop offset='0' stop-color='#ffe0a8'/><stop offset='.45' stop-color='#ff8f5a'/><stop offset='1' stop-color='#ef3f7c'/>
        </radialGradient>
        <radialGradient id='g' cx='.5' cy='.5' r='.5'>
          <stop offset='0' stop-color='#ff8a63' stop-opacity='.6'/><stop offset='1' stop-color='#ff8a63' stop-opacity='0'/>
        </radialGradient>
      </defs>
      <rect width='600' height='600' fill='url(#s)'/>
      ${STARS}
      <circle cx='372' cy='252' r='250' fill='url(#g)'/>
      <ellipse cx='372' cy='252' rx='228' ry='62' fill='none' stroke='#fff' stroke-opacity='.28' stroke-width='2' transform='rotate(-16 372 252)'/>
      <circle cx='372' cy='252' r='118' fill='url(#p)'/>
      <path d='M146 300a228 62 0 0 0 452 -6' fill='none' stroke='#fff' stroke-opacity='.75' stroke-width='3' transform='rotate(-16 372 252)'/>
      <path d='M108 452 222 404 160 470 148 444z' fill='#fff' opacity='.92'/>
      <path d='M148 444 222 404' stroke='#c7b8ff' stroke-width='2'/>
      <path d='M0 520c110-40 210-24 300-34s190-44 300-22v136H0z' fill='#120a2c' opacity='.82'/>
      <path d='M0 556c140-30 260-6 360-16s170-30 240-18v78H0z' fill='#0b0620'/>
    </svg>`),
  glassWeather: svgUrl(`
    <svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 600 600'>
      <defs>
        <linearGradient id='b' x1='0' y1='1' x2='1' y2='0'>
          <stop offset='0' stop-color='#0b3442'/><stop offset='.55' stop-color='#17808f'/><stop offset='1' stop-color='#a6eadc'/>
        </linearGradient>
        <radialGradient id='o' cx='.5' cy='.5' r='.5'>
          <stop offset='0' stop-color='#fffbe6'/><stop offset='.7' stop-color='#ffe9a8'/><stop offset='1' stop-color='#ffd36b' stop-opacity='0'/>
        </radialGradient>
      </defs>
      <rect width='600' height='600' fill='url(#b)'/>
      <circle cx='420' cy='170' r='96' fill='url(#o)'/>
      <g fill='#fff'>
        <path d='M0 0h260L120 230z' opacity='.08'/>
        <path d='M260 0h200L330 300z' opacity='.12'/>
        <path d='M120 230 330 300 180 600H0V420z' opacity='.06'/>
        <path d='M330 300 600 260v340H180z' opacity='.1'/>
        <path d='M460 0h140v260L330 300z' opacity='.05'/>
      </g>
      <g stroke='#fff' stroke-width='2' stroke-linecap='round' opacity='.35'>
        <path d='M80 300l-18 46M150 380l-18 46M240 330l-18 46M520 400l-18 46M440 480l-18 46M90 500l-18 46M300 470l-18 46'/>
      </g>
      <path d='M120 230 330 300M330 300 180 600M330 300 600 260M260 0l70 300' stroke='#fff' stroke-opacity='.4' stroke-width='1.5' fill='none'/>
    </svg>`),
  northbound: svgUrl(`
    <svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 600 600'>
      <defs>
        <linearGradient id='n' x1='0' y1='0' x2='0' y2='1'>
          <stop offset='0' stop-color='#071d26'/><stop offset='.6' stop-color='#14524b'/><stop offset='1' stop-color='#3f8c6a'/>
        </linearGradient>
      </defs>
      <rect width='600' height='600' fill='url(#n)'/>
      ${STARS}
      <circle cx='430' cy='160' r='70' fill='#f4e9c6'/>
      <circle cx='458' cy='140' r='64' fill='#0c2d33'/>
      <g fill='#06161b'>
        <path d='M40 600 90 380l50 220zM120 600l60-280 60 280zM230 600l40-200 40 200zM360 600l55-250 55 250zM470 600l45-190 45 190z'/>
      </g>
    </svg>`),
  driftwood: svgUrl(`
    <svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 600 600'>
      <defs>
        <linearGradient id='d' x1='0' y1='0' x2='0' y2='1'>
          <stop offset='0' stop-color='#f7cf9c'/><stop offset='.5' stop-color='#e68a6e'/><stop offset='1' stop-color='#7d4568'/>
        </linearGradient>
      </defs>
      <rect width='600' height='600' fill='url(#d)'/>
      <circle cx='300' cy='250' r='110' fill='#fff2d6' opacity='.85'/>
      <g fill='none' stroke='#fff' stroke-linecap='round' stroke-width='5'>
        <path d='M40 380c60-28 110-28 170 0s110 28 170 0 110-28 180 0' opacity='.7'/>
        <path d='M20 440c60-28 110-28 170 0s110 28 170 0 110-28 220 0' opacity='.5'/>
        <path d='M40 500c60-28 110-28 170 0s110 28 170 0 110-28 180 0' opacity='.32'/>
      </g>
    </svg>`),
};

/**
 * Demo songs. `apps` gives the app id each operating system's source would
 * report for the player.
 */
export const SONGS = [
  {
    title: 'Paper Satellites',
    artist: 'Juniper & The Lowlights',
    album: 'Signals After Dark',
    durationMs: 214_000,
    artwork: COVERS.paperSatellites,
    player: 'spotify',
    lyrics: {
      state: 'found',
      synced: true,
      instrumental: false,
      source: 'lrclib',
      lines: lrc(`
[00:12.40] We folded maps into paper planes
[00:16.90] And threw them out of the seventh floor
[00:21.30] The city hummed in a borrowed key
[00:25.80] I didn't know what we were waiting for
[00:30.20] Static on the radio, a song we almost knew
[00:35.10] Every station played a little bit of you
[00:39.60]
[00:44.00] So light it up, paper satellites
[00:48.30] Spinning on the wire of a summer night
[00:52.70] We're only signals crossing in the dark
[00:57.10] Finding each other by the sound of our hearts
[01:01.50] Light it up, paper satellites
[01:05.90] Every little orbit brings you back in sight
[01:10.40]
[01:18.00] Your jacket smelled like the first of rain
[01:22.40] We counted trains instead of hours
[01:26.80] You wrote your number on a parking stub
[01:31.20] I kept it folded with the dried-up flowers
[01:35.60] Static on the radio, a song we almost knew
[01:40.40] Every station played a little bit of you
[01:45.00]
[01:49.20] So light it up, paper satellites
[01:53.50] Spinning on the wire of a summer night
[01:57.90] We're only signals crossing in the dark
[02:02.30] Finding each other by the sound of our hearts
[02:06.70]
[02:24.00] And if the sky forgets our names
[02:28.40] We'll trace them out in satellite light
[02:32.80] Two little signals flickering the same
[02:37.20] Hold on, hold on through the night
[02:41.60] So light it up, paper satellites
[02:46.00] Spinning on the wire of a summer night
[02:50.40] We're only signals crossing in the dark
[02:54.80] Finding each other by the sound of our hearts
[02:59.20] Light it up, paper satellites
[03:03.60] Every little orbit brings you back in sight
[03:08.00]
[03:20.00] Paper satellites
[03:26.00]
`),
    },
  },
  {
    title: 'Glass Weather',
    artist: 'The Quiet Hours Club',
    album: 'Glass Weather',
    durationMs: 178_000,
    artwork: COVERS.glassWeather,
    player: 'spotify',
    lyrics: {
      state: 'found',
      synced: true,
      instrumental: false,
      source: 'local',
      lines: lrc(`
[00:09.50] Morning came in sideways through the blinds
[00:14.00] Coffee going cold beside your letters
[00:18.60] Every forecast said that we'd be fine
[00:23.10] Nobody predicted glass weather
[00:27.80]
[00:31.00] Oh, it's clear until it shatters
[00:35.40] Bright until it breaks
[00:39.90] Hold me like it matters
[00:44.30] Every single day
[00:48.80]
[00:56.00] Puddles full of borrowed sky
[01:00.40] We step around the pieces
[01:04.90] Umbrella made for one, but I
[01:09.30] Could learn to share the reasons
[01:13.80]
[01:17.00] Oh, it's clear until it shatters
[01:21.40] Bright until it breaks
[01:25.90] Hold me like it matters
[01:30.30] Every single day
[01:34.80]
[01:52.00] Glass weather, glass weather
[01:56.40] Careful how you hold the light
[02:00.90] Glass weather, we're together
[02:05.30] Till the morning comes out right
[02:09.80] Oh, it's clear until it shatters
[02:14.20] Bright until it breaks
[02:18.70] Hold me like it matters
[02:23.10] Every single day
[02:27.60]
`),
    },
  },
  {
    title: 'Northbound Lullaby',
    artist: 'Ada Winterline',
    album: 'Small Lights',
    durationMs: 196_000,
    artwork: COVERS.northbound,
    player: 'browser',
    lyrics: {
      state: 'found',
      synced: false,
      instrumental: false,
      source: 'lrclib',
      lines: spread(
        [
          'Pack the stars in a paper bag',
          'Leave the porch light on for me',
          'Every mile is a song we had',
          'Humming low on the frozen sea',
          'Northbound, northbound',
          'Wheels are singing me to sleep',
          'Northbound, northbound',
          'Hold the quiet I can keep',
          'Pines are counting all the cars',
          'Snow is writing out your name',
          'I will meet you where you are',
          'Northbound, all the same',
        ],
        196_000,
      ),
    },
  },
  {
    title: 'Driftwood Interlude',
    artist: 'Juniper & The Lowlights',
    album: 'Signals After Dark',
    durationMs: 132_000,
    artwork: COVERS.driftwood,
    player: 'spotify',
    lyrics: { state: 'found', synced: true, instrumental: true, source: 'lrclib', lines: [] },
  },
];

/** The app id each source reports for a demo player. */
export const PLAYER_IDS = {
  spotify: { windows: 'Spotify.exe', macos: 'com.spotify.client', linux: 'org.mpris.MediaPlayer2.spotify' },
  browser: { windows: 'msedge.exe', macos: 'com.apple.Music', linux: 'org.mpris.MediaPlayer2.firefox.instance_1_42' },
};
