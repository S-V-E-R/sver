"use client";
import { useCallback, useLayoutEffect, useRef, type RefObject } from "react";
import type { AnimationSequence } from "motion";

type Motion = typeof import("motion");

const STYLES = ["opacity", "transform", "stroke-dashoffset"];

/**
 * Plays one of the two signature moments (docs/MOTION.md) inside `root`. The final state is already
 * rendered; this only plays it in. Motion is imported here, on demand, so no other page downloads it.
 * Elements marked `data-play` start hidden. Reduced motion, a failed import, Skip, Escape and the
 * natural end all land on the same final state: the plain CSS one, because every inline style the
 * sequence leaves is cleared. End keyframes must be explicit ("scale(1)", not "none"): Motion
 * reads "none" as zeros. Returns `skip`, for the Skip button.
 */
export function useChoreography(root: RefObject<HTMLElement | null>, sequence: ((root: HTMLElement, motion: Motion) => AnimationSequence) | null, onEnd: () => void) {
  const end = useRef(onEnd);
  const skip = useRef<() => void>(() => {});
  useLayoutEffect(() => { end.current = onEnd; });
  useLayoutEffect(() => {
    const el = root.current;
    if (!sequence || !el) return;
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) { end.current(); return; }
    const parts = [...el.querySelectorAll<HTMLElement | SVGElement>("[data-play]")];
    for (const part of parts) part.style.opacity = "0";
    let controls: { complete(): void; stop(): void } | null = null;
    let done = false;
    const clear = () => {
      done = true;
      window.removeEventListener("keydown", key);
      // Finished animations keep holding their last frame; cancel them so the CSS state shows.
      for (const animation of el.getAnimations({ subtree: true })) animation.cancel();
      for (const part of parts) for (const style of STYLES) part.style.removeProperty(style);
    };
    const finish = () => {
      if (done) return;
      controls?.complete();
      controls?.stop();
      clear();
      end.current();
    };
    const key = (event: KeyboardEvent) => { if (event.key === "Escape") finish(); };
    window.addEventListener("keydown", key);
    skip.current = finish;
    import("motion").then(motion => {
      if (done) return;
      const playing = motion.animate(sequence(el, motion));
      controls = playing;
      playing.then(finish, finish);
    }, finish);
    return () => { controls?.stop(); clear(); };
  }, [root, sequence]);
  return useCallback(() => skip.current(), []);
}
