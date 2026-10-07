// Small actions shared by several pages.

import { isDemo } from './api.js';
import { attempt, toast } from './dom.js';

const FOLDER_NAMES = {
  config: 'the settings folder',
  lyrics: 'your lyrics folder',
  cache: 'the saved lyrics folder',
  logs: 'the logs folder',
};

/** Opens one of Lyrix's folders in the file manager. */
export async function openFolder(api, which) {
  const result = await attempt(api.openFolder(which), 'Could not open the folder');
  if (result !== undefined && isDemo) {
    toast(`Demo: this opens ${FOLDER_NAMES[which]}`, { iconName: 'folder' });
  }
}

/** Opens an https:// link in the browser. */
export function openUrl(api, url) {
  return attempt(api.openUrl(url), 'Could not open the link');
}
