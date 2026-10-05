import { useCallback, useEffect, useRef, useState } from "react";

/** Distance from the bottom, in pixels, that still counts as "at the end". */
const threshold = 96;

/**
 * Keep a scrolling region pinned to its newest content without stealing the
 * wheel from a reader who scrolled up.
 *
 * `followKey` should change whenever the content grows — a length, a version
 * string, anything monotonic.
 */
export function useStickToBottom<T extends HTMLElement>(followKey: string | number) {
  const ref = useRef<T>(null);
  const [pinned, setPinned] = useState(true);

  const trackScroll = useCallback(() => {
    const area = ref.current;
    if (!area) {
      return;
    }
    setPinned(area.scrollHeight - area.scrollTop - area.clientHeight < threshold);
  }, []);

  const scrollToBottom = useCallback((smooth = false) => {
    const area = ref.current;
    if (!area) {
      return;
    }
    if (smooth) {
      area.scrollTo({ top: area.scrollHeight, behavior: "smooth" });
    } else {
      area.scrollTop = area.scrollHeight;
    }
    setPinned(true);
  }, []);

  useEffect(() => {
    const area = ref.current;
    if (area && pinned) {
      area.scrollTop = area.scrollHeight;
    }
  }, [followKey, pinned]);

  return { ref, pinned, trackScroll, scrollToBottom };
}
