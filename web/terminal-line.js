// One line of transcript output - shared by page.js (the wired demo's
// two panels and the /originate|/receive chat log) and explained.js (the
// overture's own transcript). Pulled out of page.js so neither file
// carries its own copy that could drift from the other's.
//
// The typewriter reveal (`.terminal-line--reveal`, applied to command
// lines) is purely decorative timing - the full text is set on the
// element immediately via textContent, so a screen reader gets the whole
// line the instant it appears; see style.css's own doc on the CSS side
// of this (a clip-path animation, not a content reveal, and disabled
// outright under prefers-reduced-motion).
export function appendTerminalLine(container, text, { command = false } = {}) {
  const line = document.createElement('p');
  line.className = 'terminal-line' + (command ? ' terminal-line--command terminal-line--reveal' : ' terminal-line--phase');
  line.textContent = text;
  container.appendChild(line);
  container.scrollTop = container.scrollHeight;
  return line;
}
