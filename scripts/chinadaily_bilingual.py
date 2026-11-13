#!/usr/bin/env python3
"""抓取 China Daily 双语新闻/双语百科，转成 quietly 的阅读文本。

用途仅限个人学习：脚本把抓到的正文写到本地目录（默认 `packs/texts/chinadaily/`，
已在 .gitignore 里排除），不要把结果提交进仓库或二次分发——正文版权归中国日报。

输出格式与 `quietly read` 的约定一致：

    # title: <标题>
    # book: China Daily Bilingual
    # source: <原文链接>
    # license: personal study only · do not redistribute

    <英文段落，空行分隔>

    ---

    <中文段落，空行分隔>

原文是「英文一段、中文一段」交替排列，脚本按顺序配对；配不上的中文段落（开头的
导语、文末的编辑/校对署名）直接丢掉，所以不会出现「英文第 3 段对中文第 2 段」。
"""

from __future__ import annotations

import argparse
import html
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

BASE = "https://language.chinadaily.com.cn"
LIST_URL = BASE + "/news_bilingual"
LIST_PAGE_URL = BASE + "/news_bilingual/page_{page}.html"
USER_AGENT = "Mozilla/5.0 (compatible; quietly-reader/0.1; personal study)"

ARTICLE_HREF = re.compile(r'href="(//language\.chinadaily\.com\.cn/a/\d{6}/\d{2}/[^"]+?\.html)"')
BLOCK_TAG = re.compile(r"<p[^>]*>(.*?)</p>", re.S | re.I)
TAG = re.compile(r"<[^>]+>")
NOISE = re.compile(r"^\s*(编辑|校对|来源|监制|责编|Source|Editor|Proofreader)", re.I)


def fetch(url: str, retries: int = 3) -> str:
    """取一页 HTML；失败重试，最终仍失败就抛错。"""
    last: Exception | None = None
    for attempt in range(retries):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(request, timeout=30) as response:
                return response.read().decode("utf-8", "replace")
        except (urllib.error.URLError, TimeoutError) as exc:  # 网络抖动
            last = exc
            time.sleep(1.5 * (attempt + 1))
    raise RuntimeError(f"抓取失败：{url}（{last}）")


def article_links(pages: int) -> list[str]:
    """列表页里的文章链接，按出现顺序去重。"""
    urls: list[str] = []
    seen: set[str] = set()
    for page in range(1, pages + 1):
        url = LIST_URL if page == 1 else LIST_PAGE_URL.format(page=page)
        raw = fetch(url)
        for href in ARTICLE_HREF.findall(raw):
            full = "https:" + href if href.startswith("//") else href
            if full not in seen:
                seen.add(full)
                urls.append(full)
        print(f"  page {page}: {len(urls)} links so far", file=sys.stderr)
        time.sleep(1.0)
    return urls


def clean(text: str) -> str:
    text = TAG.sub("", text)
    text = html.unescape(text)
    text = text.replace("\u3000", " ").replace("\u00a0", " ").replace("\u200b", "")
    return re.sub(r"[ \t]+", " ", text).strip()


def paragraphs(body: str) -> list[str]:
    """正文里的段落，丢掉空行与署名行。

    两个容易踩的坑，都是实测出来的：

    * 中文段落常被 `<a>` 整段包起来（站点的样式需要），所以**不能**因为段落里含
      链接就当成「相关阅读」丢掉，链接标签剥掉、文字保留即可；
    * 一个 `<p>` 里可能用 `<br>` 塞了多段中文，要按它再切开，否则中文段数永远
      少于英文段数、逐段对齐就无从谈起。
    """
    out: list[str] = []
    for raw in BLOCK_TAG.findall(body):
        raw = re.sub(r"<br\s*/?>", "\n\n", raw, flags=re.I)
        for chunk in raw.split("\n\n"):
            text = clean(chunk)
            if not text or NOISE.match(text):
                continue
            out.append(text)
    return out


def is_chinese(text: str) -> bool:
    cjk = sum(1 for ch in text if "\u4e00" <= ch <= "\u9fff")
    return cjk / max(1, len(text)) > 0.3


def article_body(raw: str) -> str:
    """`id="Content"` 到脚本/图片区之前的部分。"""
    start = raw.find('id="Content"')
    if start < 0:
        return ""
    region = raw[start:]
    for marker in ("<script", 'class="main_img"', 'class="top_share"'):
        cut = region.find(marker)
        if cut > 0:
            region = region[:cut]
    return region


def one_line(text: str) -> str:
    """把标题压成一行。

    站点的 `<h1>` 里有时用 `<br>`/换行把中文主标题和英文副标题分开；不压平的话
    写出来的 `# title:` 只吃到第一行，第二行会被当成正文，后面的 `# book:` /
    `# source:` 就一起失效（导入后成了没有归书的散篇）。
    """
    return " ".join(text.split())


def article_title(raw: str, fallback: str) -> str:
    match = re.search(r"<h1[^>]*>(.*?)</h1>", raw, re.S | re.I)
    if match:
        title = one_line(clean(match.group(1)))
        if title:
            return title
    match = re.search(r"<title>(.*?)</title>", raw, re.S | re.I)
    if match:
        # 常见形态：`标题 - Chinadaily.com.cn`
        title = one_line(re.split(r"\s+-\s+", clean(match.group(1)))[0])
        if title:
            return title
    return fallback


def pair(paragraphs: list[str]) -> tuple[list[str], list[str], bool]:
    """英文段落与中文段落配对，返回 `(english, chinese, aligned)`。

    站点有两种排版：交错（英文一段、中文一段）与成块（先整段英文，再整段中文）。
    两者在「段数相等」时按顺序配对都成立，所以先比段数；不等时退回到「紧跟英文
    之后的中文」这个更严格的条件。仍对不上就说明译文把几段并成了一段，交给调用方
    决定跳过——宁可少一篇，也不要让 `translation_at(i)` 指到错误的段落。
    """
    kinds = ["Z" if is_chinese(text) else "E" for text in paragraphs]
    english = [text for text, kind in zip(paragraphs, kinds) if kind == "E"]
    chinese = [text for text, kind in zip(paragraphs, kinds) if kind == "Z"]
    if len(english) == len(chinese):
        return english, chinese, True

    adjacent: list[tuple[str, str]] = []
    for index, kind in enumerate(kinds[:-1]):
        if kind == "E" and kinds[index + 1] == "Z":
            adjacent.append((paragraphs[index], paragraphs[index + 1]))
    if adjacent and len(adjacent) == len(english):
        return [en for en, _ in adjacent], [zh for _, zh in adjacent], True
    return english, chinese, False


def slug(url: str) -> str:
    match = re.search(r"/a/(\d{4})(\d{2})/(\d{2})/(\w+)\.html", url)
    if not match:
        return re.sub(r"\W+", "-", url)[-40:]
    year, month, day, article_id = match.groups()
    return f"{year}{month}{day}-{article_id.lower()}"


def write_text(url: str, title: str, english: list[str], chinese: list[str], out: Path) -> Path:
    target = out / f"{slug(url)}.txt"
    header = (
        f"# title: {one_line(title)}\n"
        f"# book: China Daily Bilingual\n"
        f"# source: {url}\n"
        f"# license: personal study only · do not redistribute\n"
    )
    body = "\n\n".join(english) + "\n\n---\n\n" + "\n\n".join(chinese) + "\n"
    target.write_text(header + "\n" + body, encoding="utf-8")
    return target


def main() -> int:
    parser = argparse.ArgumentParser(description="抓取 China Daily 双语新闻，转成 quietly 阅读文本")
    parser.add_argument("--pages", type=int, default=1, help="列表页翻几页（每页约 20 篇）")
    parser.add_argument("--limit", type=int, default=5, help="最多抓几篇（0 表示不限）")
    parser.add_argument("--out", default="packs/texts/chinadaily", help="输出目录")
    parser.add_argument("--sleep", type=float, default=1.0, help="每篇之间的间隔秒数")
    args = parser.parse_args()

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    print(f"listing {args.pages} page(s) ...", file=sys.stderr)
    links = article_links(args.pages)
    if args.limit:
        links = links[: args.limit]
    print(f"{len(links)} article(s) to fetch", file=sys.stderr)

    saved = skipped = failed = 0
    for index, url in enumerate(links, 1):
        try:
            raw = fetch(url)
            english, chinese, aligned = pair(paragraphs(article_body(raw)))
            if not english or not chinese:
                print(f"[{index}] skip (no bilingual body): {url}", file=sys.stderr)
                skipped += 1
                continue
            if not aligned:
                # 译文把几段并成一段，按段对齐就无从谈起；跳过而不是错位。
                print(
                    f"[{index}] skip (cannot align: {len(english)} EN vs {len(chinese)} ZH): {url}",
                    file=sys.stderr,
                )
                skipped += 1
                continue
            title = article_title(raw, slug(url))
            path = write_text(url, title, english, chinese, out)
            print(f"[{index}] {path.name} · {len(english)} pairs · {sum(len(p) for p in english)} chars")
            saved += 1
        except Exception as exc:  # 单篇失败不影响其余
            print(f"[{index}] failed: {url} ({exc})", file=sys.stderr)
            failed += 1
        time.sleep(args.sleep)

    print(f"\nsaved {saved} · skipped {skipped} · failed {failed} -> {out}", file=sys.stderr)
    return 0 if saved else 1


if __name__ == "__main__":
    sys.exit(main())
