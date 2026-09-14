#!/usr/bin/env python3
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PARSER = ROOT / "libsurfer" / "src" / "command_parser.rs"
DOCS = ROOT / "docs" / "commands" / "README.md"

MATCH_QUERY = "match query {"
TOP_LEVEL_MATCH_ARM_RE = re.compile(r'^ {16}(?:"[a-z0-9_]+"\s*(?:\|\s*)?)+\s*=>')
COMMAND_RE = re.compile(r'"([a-z0-9_]+)"')
DOC_CODE_RE = re.compile(r'``([^`]+)``')
COMMAND_NAME_RE = re.compile(r'^[a-z][a-z0-9_]*$')


def extract_parser_commands(text: str) -> set[str]:
    """Commands in the top-level `match query` arms.

    Only arms at the match's own brace depth are commands; nested `match`
    statements inside an arm (e.g. parsing a setting value) are not. String
    literals and line comments are skipped while tracking the depth so that
    braces in format strings do not confuse the scan.
    """
    marker = text.index(MATCH_QUERY) + len(MATCH_QUERY)
    commands: set[str] = set()
    depth = 1
    line_depth = 1
    line_start = marker
    in_string = False
    index = marker

    while index < len(text):
        char = text[index]
        if char == "\n":
            line = text[line_start:index]
            if line_depth == 1 and TOP_LEVEL_MATCH_ARM_RE.match(line):
                commands.update(COMMAND_RE.findall(line))
            line_start = index + 1
            line_depth = depth
        elif in_string:
            if char == "\\":
                index += 2
                continue
            if char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "/" and text[index + 1 : index + 2] == "/":
            newline = text.find("\n", index)
            if newline == -1:
                break
            index = newline
            continue
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                break
        index += 1

    return commands


def extract_doc_commands(text: str) -> set[str]:
    commands: set[str] = set()
    for block in DOC_CODE_RE.findall(text):
        first = block.strip().split()[0]
        if COMMAND_NAME_RE.match(first):
            commands.add(first)
    return commands


def main() -> int:
    parser_text = PARSER.read_text(encoding="utf-8")
    docs_text = DOCS.read_text(encoding="utf-8")

    parser_commands = extract_parser_commands(parser_text)
    doc_commands = extract_doc_commands(docs_text)

    missing = sorted(parser_commands - doc_commands)

    if missing:
        print("Commands missing from docs/commands/README.md:")
        for command in missing:
            print(f"- {command}")
        return 1

    print(f"All {len(parser_commands)} command_parser commands are documented.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
