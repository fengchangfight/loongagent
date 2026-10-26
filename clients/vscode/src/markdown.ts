function escapeHtml(value: string): string {
  return value.replace(/[&<>"]/g, (character) => {
    switch (character) {
      case "&": return "&amp;";
      case "<": return "&lt;";
      case ">": return "&gt;";
      default: return "&quot;";
    }
  });
}

/** A link is only followed when its protocol is http or https. */
function safeHttpUrl(url: string): string | undefined {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:" ? url : undefined;
  } catch {
    return undefined;
  }
}

/** The inline subset: code, bold, italic and links; everything is escaped first. */
function inline(raw: string): string {
  let text = escapeHtml(raw);
  const codes: string[] = [];
  text = text.replace(/`([^`]+)`/g, (_whole, code: string) => {
    codes.push(code);
    return `\u0000${codes.length - 1}\u0000`;
  });
  text = text.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  text = text.replace(/\*([^*]+)\*/g, "<em>$1</em>");
  text = text.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (whole, label: string, url: string) => {
    const safe = safeHttpUrl(url);
    return safe === undefined ? whole : `<a href="${safe}" rel="noreferrer">${label}</a>`;
  });
  return text.replace(/\u0000(\d+)\u0000/g, (_whole, index: string) => `<code>${codes[Number(index)]}</code>`);
}

type ListMatch = { ordered: boolean; text: string };

function listItem(line: string): ListMatch | undefined {
  const match = /^\s*([-*]|\d+\.)\s+(.*)$/.exec(line);
  if (match === null) return undefined;
  return { ordered: match[1].endsWith("."), text: match[2] };
}

/**
 * Render the Markdown subset the sidebar promises.
 *
 * Raw HTML is escaped before any construct is recognised, so a model cannot
 * inject an element. Fenced code is copied as text; an unmatched fence is
 * ordinary text rather than swallowing the rest of the answer.
 */
export function renderMarkdown(text: string): string {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const out: string[] = [];
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];
    const opening = /^(`{3,})\s*([^\s`]*)\s*$/.exec(line);
    if (opening !== null) {
      const ticks = opening[1].length;
      const language = opening[2];
      const body: string[] = [];
      const start = index;
      let closed = false;
      index++;
      while (index < lines.length) {
        if (new RegExp("^`{" + ticks + ",}\\s*$").test(lines[index])) {
          closed = true;
          index++;
          break;
        }
        body.push(lines[index]);
        index++;
      }
      if (!closed) {
        index = start + 1;
        out.push(`<p>${inline(line)}</p>`);
        continue;
      }
      const languageClass = language === "" ? "" : ` class="language-${escapeHtml(language)}"`;
      out.push(`<pre><code${languageClass}>${escapeHtml(body.join("\n"))}</code></pre>`);
      continue;
    }

    if (line.trim() === "") {
      index++;
      continue;
    }

    const item = listItem(line);
    if (item !== undefined) {
      const ordered = item.ordered;
      const items: string[] = [];
      while (index < lines.length) {
        const next = listItem(lines[index]);
        if (next === undefined || next.ordered !== ordered) break;
        items.push(next.text);
        index++;
      }
      const tag = ordered ? "ol" : "ul";
      out.push(`<${tag}>${items.map((entry) => `<li>${inline(entry)}</li>`).join("")}</${tag}>`);
      continue;
    }

    out.push(`<p>${inline(line)}</p>`);
    index++;
  }

  return out.join("\n");
}
