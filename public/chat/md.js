/* Markdown to HTML for chat replies. Everything is escaped first and markup is
   built only from this file's own tags, so text from the model, a tool or a
   file can never become markup. Free of DOM so node --test can exercise it;
   the IIFE form makes it both a classic script and a side-effect ES module. */
(function (g) {
  const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
  const esc = s => s.replace(/[&<>"']/g, c => ESC[c]);
  const unesc = s => s.replace(/&(amp|lt|gt|quot|#39);/g, (_, e) => ({ amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" })[e]);
  const safeUrl = url => /^(https?:\/\/|mailto:)/.test(unesc(url).trim().toLowerCase());

  // Input is already escaped. Code spans and links are set aside first so the
  // emphasis rules cannot reach inside them.
  function inline(t) {
    const kept = [];
    const hold = html => '\u0000' + (kept.push(html) - 1) + '\u0000';
    t = t.replace(/`([^`]+)`/g, (_, c) => hold('<code>' + c + '</code>'));
    t = t.replace(/\[([^[\]]*)\]\(([^()\s]*)\)/g, (m, text, url) => safeUrl(url)
      ? hold('<a href="' + url + '" target="_blank" rel="noopener noreferrer">' + text + '</a>')
      : m);
    t = t.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
    t = t.replace(/\*([^*\s][^*]*)\*/g, '<em>$1</em>');
    return t.replace(/\u0000(\d+)\u0000/g, (_, i) => kept[i]);
  }

  const FENCE = /^\s*```/;
  const HEADING = /^(#{1,6})\s+(.*)$/;
  const RULE = /^\s*(-{3,}|\*{3,}|_{3,})\s*$/;
  const UL = /^\s*[-*+]\s+(.*)$/;
  const OL = /^\s*\d+[.)]\s+(.*)$/;
  const QUOTE = /^\s*&gt;\s?(.*)$/;
  const TABLE_SEP = /^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?\s*$/;
  const cells = l => l.trim().replace(/^\|/, '').replace(/\|$/, '').split('|').map(c => inline(c.trim()));
  const startsBlock = (l, next) => FENCE.test(l) || HEADING.test(l) || RULE.test(l) || UL.test(l) || OL.test(l)
    || QUOTE.test(l) || (l.includes('|') && next !== undefined && TABLE_SEP.test(next));

  function blocks(lines) {
    let out = '';
    for (let i = 0; i < lines.length;) {
      const l = lines[i];
      if (!l.trim()) { i++; continue; }
      if (FENCE.test(l)) {
        // An unterminated fence (a reply still being written) runs to the end.
        const lang = l.replace(FENCE, '').replace(/[^A-Za-z0-9_+-]/g, '');
        let body = '';
        for (i++; i < lines.length && !/^\s*```\s*$/.test(lines[i]); i++) body += lines[i] + '\n';
        i++;
        out += '<pre><code' + (lang ? ' class="lang-' + lang + '"' : '') + '>' + body + '</code></pre>';
        continue;
      }
      let m = l.match(HEADING);
      if (m) { out += '<h' + m[1].length + '>' + inline(m[2]) + '</h' + m[1].length + '>'; i++; continue; }
      if (RULE.test(l)) { out += '<hr>'; i++; continue; }
      if (l.includes('|') && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1])) {
        out += '<table><thead><tr>' + cells(l).map(c => '<th>' + c + '</th>').join('') + '</tr></thead><tbody>';
        for (i += 2; i < lines.length && lines[i].includes('|'); i++) {
          out += '<tr>' + cells(lines[i]).map(c => '<td>' + c + '</td>').join('') + '</tr>';
        }
        out += '</tbody></table>';
        continue;
      }
      const list = UL.test(l) ? ['ul', UL] : OL.test(l) ? ['ol', OL] : null;
      if (list) {
        out += '<' + list[0] + '>';
        for (; i < lines.length && (m = lines[i].match(list[1])); i++) out += '<li>' + inline(m[1]) + '</li>';
        out += '</' + list[0] + '>';
        continue;
      }
      if (QUOTE.test(l)) {
        const inner = [];
        for (; i < lines.length && (m = lines[i].match(QUOTE)); i++) inner.push(m[1]);
        out += '<blockquote>' + blocks(inner) + '</blockquote>';
        continue;
      }
      const para = [l];
      for (i++; i < lines.length && lines[i].trim() && !startsBlock(lines[i], lines[i + 1]); i++) para.push(lines[i]);
      out += '<p>' + para.map(inline).join('<br>') + '</p>';
    }
    return out;
  }

  g.cdashMd = {
    render(text) {
      if (!text) return '';
      return blocks(esc(String(text).replace(/\u0000/g, '')).split('\n'));
    },
  };
})(typeof globalThis === 'undefined' ? this : globalThis);
