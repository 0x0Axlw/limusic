// The music video's state, and the handoff of the one live <video> element.
//
// It lives outside NowPlaying.svelte because +layout unmounts that component whenever the player
// view is closed. Rebuilding the element on every reopen meant opening the stream at byte 0 and
// then seeking minutes into a WebM, which has to fetch the container index first and takes seconds
// that no amount of tuning the sync loop could recover. So the element is owned by VideoSurface,
// which is always mounted, and the view borrows it.
import * as api from './api';
import { playback, prefs, videoReady } from './player.svelte';

/** Session-sticky on purpose: someone who hits "show artwork" wants artwork now and almost
 *  certainly on the next video too, but a permanent no is what the setting is for. */
export const video = $state({
	want: true,
	/** Whether the player view is holding the element, i.e. someone can actually see the picture.
	 *  VideoSurface releases the stream when nobody can and the music is not moving. */
	shown: false,
	/** The loopback proxy URL for the current track, or null. Owned by VideoSurface's fetch. */
	url: null as string | null,
	/** Linux: the hole the page leaves for mpv's picture, while the picture is up (see setHole).
	 *  Viewport CSS pixels. */
	hole: null as Hole | null
});

export type Hole = { x: number; y: number; w: number; h: number };

export const canVideo = () => prefs.musicVideos && !!playback.now?.isVideo;
export const hasVideo = () =>
	canVideo() && (prefs.nativeVideo ? !!videoReady[playback.now!.videoId] : !!video.url);
export const showVideo = () => hasVideo() && video.want;

// The live element and where it waits when nothing is showing it. Plain module lets: this is DOM
// identity, nothing renders off it.
let node: HTMLVideoElement | null = null;
let parking: HTMLElement | null = null;

/** VideoSurface, once, at mount. */
export function registerVideo(v: HTMLVideoElement, park: HTMLElement) {
	node = v;
	parking = park;
	park.appendChild(v);
}

/** Put the picture in `box`. Synchronous on purpose: a media element that is out of the document
 *  across a microtask gets paused by the spec's removal steps, and a paused picture is exactly the
 *  desync this whole thing exists to avoid. */
export function claimVideo(box: HTMLElement) {
	if (node) box.appendChild(node);
	video.shown = true;
}

/** Send it back to the parking container. Same rule: synchronous, never from an effect. */
export function parkVideo() {
	if (node && parking) parking.appendChild(node);
	video.shown = false;
}

// --- Linux: mpv draws the picture under the page (src-tauri/src/nativevideo.rs) ----------------
// The window is transparent, so a hole with nothing under it shows the desktop. The hole therefore
// opens only once Rust says the picture is in place, and closes before Rust takes it away.

let holeSeq = 0;
let holeSent: string | null = null;

/** Put mpv's picture at `r` (viewport CSS pixels), or take it away. */
export function setHole(r: Hole | null) {
	const key = r && `${r.x},${r.y},${r.w},${r.h}`;
	if (key === holeSent) return;
	holeSent = key;
	const seq = ++holeSeq;
	if (!r) {
		video.hole = null;
		api.nativeVideoRect(null).catch(() => {});
		return;
	}
	// Already up and only moving: move the hole with it rather than a round trip behind.
	if (video.hole) video.hole = r;
	api.nativeVideoRect([r.x, r.y, r.w, r.h])
		.then((up) => {
			if (seq !== holeSeq) return;
			video.hole = up ? r : null;
			// No GL surface, and there never will be: back to the <video> element.
			if (!up) prefs.nativeVideo = false;
		})
		.catch(() => {});
}
