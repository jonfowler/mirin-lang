// External scanner: the raw body of `= verilog { … }` as ONE token.
// Brace-counting from after the opening `{` (verilog concatenation braces
// are balanced), skipping double-quoted strings and `//`/`/* */` comments so
// braces inside them don't miscount. The token ends BEFORE the matching
// close brace. `${…}` splices are plain content here — the compiler splits
// them during body lowering.

#include "tree_sitter/parser.h"

#include <stdbool.h>

enum TokenType { VERILOG_CONTENT };

void *tree_sitter_mirin_external_scanner_create(void) { return NULL; }
void tree_sitter_mirin_external_scanner_destroy(void *payload) { (void)payload; }
unsigned tree_sitter_mirin_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}
void tree_sitter_mirin_external_scanner_deserialize(void *payload, const char *buffer,
                                                    unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

bool tree_sitter_mirin_external_scanner_scan(void *payload, TSLexer *lexer,
                                             const bool *valid_symbols) {
  (void)payload;
  if (!valid_symbols[VERILOG_CONTENT]) {
    return false;
  }
  int depth = 0;
  for (;;) {
    if (lexer->eof(lexer)) {
      return false; // unterminated block — let the parser error
    }
    int32_t c = lexer->lookahead;
    if (c == '}') {
      if (depth == 0) {
        break; // the block's own close brace — not part of the content
      }
      depth--;
      advance(lexer);
    } else if (c == '{') {
      depth++;
      advance(lexer);
    } else if (c == '"') {
      advance(lexer);
      while (!lexer->eof(lexer) && lexer->lookahead != '"') {
        if (lexer->lookahead == '\\') {
          advance(lexer);
          if (lexer->eof(lexer)) {
            return false;
          }
        }
        advance(lexer);
      }
      if (!lexer->eof(lexer)) {
        advance(lexer); // closing quote
      }
    } else if (c == '/') {
      advance(lexer);
      if (lexer->lookahead == '/') {
        while (!lexer->eof(lexer) && lexer->lookahead != '\n') {
          advance(lexer);
        }
      } else if (lexer->lookahead == '*') {
        advance(lexer);
        int32_t prev = 0;
        while (!lexer->eof(lexer) && !(prev == '*' && lexer->lookahead == '/')) {
          prev = lexer->lookahead;
          advance(lexer);
        }
        if (!lexer->eof(lexer)) {
          advance(lexer); // the '/'
        }
      }
    } else {
      advance(lexer);
    }
  }
  lexer->result_symbol = VERILOG_CONTENT;
  return true;
}
