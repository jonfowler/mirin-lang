#include "tree_sitter/parser.h"

#if defined(__GNUC__) || defined(__clang__)
#pragma GCC diagnostic ignored "-Wmissing-field-initializers"
#endif

#define LANGUAGE_VERSION 14
#define STATE_COUNT 49
#define LARGE_STATE_COUNT 2
#define SYMBOL_COUNT 35
#define ALIAS_COUNT 0
#define TOKEN_COUNT 18
#define EXTERNAL_TOKEN_COUNT 0
#define FIELD_COUNT 15
#define MAX_ALIAS_SEQUENCE_LENGTH 7
#define PRODUCTION_ID_COUNT 12

enum ts_symbol_identifiers {
  anon_sym_fn = 1,
  anon_sym_DASH_GT = 2,
  anon_sym_LPAREN = 3,
  anon_sym_COMMA = 4,
  anon_sym_RPAREN = 5,
  anon_sym_COLON = 6,
  anon_sym_AT = 7,
  anon_sym_LBRACE = 8,
  anon_sym_RBRACE = 9,
  anon_sym_let = 10,
  anon_sym_EQ = 11,
  anon_sym_SEMI = 12,
  anon_sym_return = 13,
  anon_sym_PLUS = 14,
  anon_sym_DOT = 15,
  sym_identifier = 16,
  sym_line_comment = 17,
  sym_source_file = 18,
  sym_function_item = 19,
  sym_parameters = 20,
  sym_parameter = 21,
  sym_type = 22,
  sym_domain_tag = 23,
  sym_block = 24,
  sym__statement = 25,
  sym_let_declaration = 26,
  sym_return_statement = 27,
  sym__expression = 28,
  sym_binary_expression = 29,
  sym_field_expression = 30,
  sym_call_expression = 31,
  aux_sym_source_file_repeat1 = 32,
  aux_sym_parameters_repeat1 = 33,
  aux_sym_block_repeat1 = 34,
};

static const char * const ts_symbol_names[] = {
  [ts_builtin_sym_end] = "end",
  [anon_sym_fn] = "fn",
  [anon_sym_DASH_GT] = "->",
  [anon_sym_LPAREN] = "(",
  [anon_sym_COMMA] = ",",
  [anon_sym_RPAREN] = ")",
  [anon_sym_COLON] = ":",
  [anon_sym_AT] = "@",
  [anon_sym_LBRACE] = "{",
  [anon_sym_RBRACE] = "}",
  [anon_sym_let] = "let",
  [anon_sym_EQ] = "=",
  [anon_sym_SEMI] = ";",
  [anon_sym_return] = "return",
  [anon_sym_PLUS] = "+",
  [anon_sym_DOT] = ".",
  [sym_identifier] = "identifier",
  [sym_line_comment] = "line_comment",
  [sym_source_file] = "source_file",
  [sym_function_item] = "function_item",
  [sym_parameters] = "parameters",
  [sym_parameter] = "parameter",
  [sym_type] = "type",
  [sym_domain_tag] = "domain_tag",
  [sym_block] = "block",
  [sym__statement] = "_statement",
  [sym_let_declaration] = "let_declaration",
  [sym_return_statement] = "return_statement",
  [sym__expression] = "_expression",
  [sym_binary_expression] = "binary_expression",
  [sym_field_expression] = "field_expression",
  [sym_call_expression] = "call_expression",
  [aux_sym_source_file_repeat1] = "source_file_repeat1",
  [aux_sym_parameters_repeat1] = "parameters_repeat1",
  [aux_sym_block_repeat1] = "block_repeat1",
};

static const TSSymbol ts_symbol_map[] = {
  [ts_builtin_sym_end] = ts_builtin_sym_end,
  [anon_sym_fn] = anon_sym_fn,
  [anon_sym_DASH_GT] = anon_sym_DASH_GT,
  [anon_sym_LPAREN] = anon_sym_LPAREN,
  [anon_sym_COMMA] = anon_sym_COMMA,
  [anon_sym_RPAREN] = anon_sym_RPAREN,
  [anon_sym_COLON] = anon_sym_COLON,
  [anon_sym_AT] = anon_sym_AT,
  [anon_sym_LBRACE] = anon_sym_LBRACE,
  [anon_sym_RBRACE] = anon_sym_RBRACE,
  [anon_sym_let] = anon_sym_let,
  [anon_sym_EQ] = anon_sym_EQ,
  [anon_sym_SEMI] = anon_sym_SEMI,
  [anon_sym_return] = anon_sym_return,
  [anon_sym_PLUS] = anon_sym_PLUS,
  [anon_sym_DOT] = anon_sym_DOT,
  [sym_identifier] = sym_identifier,
  [sym_line_comment] = sym_line_comment,
  [sym_source_file] = sym_source_file,
  [sym_function_item] = sym_function_item,
  [sym_parameters] = sym_parameters,
  [sym_parameter] = sym_parameter,
  [sym_type] = sym_type,
  [sym_domain_tag] = sym_domain_tag,
  [sym_block] = sym_block,
  [sym__statement] = sym__statement,
  [sym_let_declaration] = sym_let_declaration,
  [sym_return_statement] = sym_return_statement,
  [sym__expression] = sym__expression,
  [sym_binary_expression] = sym_binary_expression,
  [sym_field_expression] = sym_field_expression,
  [sym_call_expression] = sym_call_expression,
  [aux_sym_source_file_repeat1] = aux_sym_source_file_repeat1,
  [aux_sym_parameters_repeat1] = aux_sym_parameters_repeat1,
  [aux_sym_block_repeat1] = aux_sym_block_repeat1,
};

static const TSSymbolMetadata ts_symbol_metadata[] = {
  [ts_builtin_sym_end] = {
    .visible = false,
    .named = true,
  },
  [anon_sym_fn] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_DASH_GT] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_LPAREN] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_COMMA] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_RPAREN] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_COLON] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_AT] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_LBRACE] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_RBRACE] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_let] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_SEMI] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_return] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_PLUS] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_DOT] = {
    .visible = true,
    .named = false,
  },
  [sym_identifier] = {
    .visible = true,
    .named = true,
  },
  [sym_line_comment] = {
    .visible = true,
    .named = true,
  },
  [sym_source_file] = {
    .visible = true,
    .named = true,
  },
  [sym_function_item] = {
    .visible = true,
    .named = true,
  },
  [sym_parameters] = {
    .visible = true,
    .named = true,
  },
  [sym_parameter] = {
    .visible = true,
    .named = true,
  },
  [sym_type] = {
    .visible = true,
    .named = true,
  },
  [sym_domain_tag] = {
    .visible = true,
    .named = true,
  },
  [sym_block] = {
    .visible = true,
    .named = true,
  },
  [sym__statement] = {
    .visible = false,
    .named = true,
  },
  [sym_let_declaration] = {
    .visible = true,
    .named = true,
  },
  [sym_return_statement] = {
    .visible = true,
    .named = true,
  },
  [sym__expression] = {
    .visible = false,
    .named = true,
  },
  [sym_binary_expression] = {
    .visible = true,
    .named = true,
  },
  [sym_field_expression] = {
    .visible = true,
    .named = true,
  },
  [sym_call_expression] = {
    .visible = true,
    .named = true,
  },
  [aux_sym_source_file_repeat1] = {
    .visible = false,
    .named = false,
  },
  [aux_sym_parameters_repeat1] = {
    .visible = false,
    .named = false,
  },
  [aux_sym_block_repeat1] = {
    .visible = false,
    .named = false,
  },
};

enum ts_field_identifiers {
  field_base_type = 1,
  field_body = 2,
  field_domain_tag = 3,
  field_field = 4,
  field_function = 5,
  field_initializer = 6,
  field_left = 7,
  field_name = 8,
  field_object = 9,
  field_operator = 10,
  field_parameters = 11,
  field_return_type = 12,
  field_right = 13,
  field_type = 14,
  field_value = 15,
};

static const char * const ts_field_names[] = {
  [0] = NULL,
  [field_base_type] = "base_type",
  [field_body] = "body",
  [field_domain_tag] = "domain_tag",
  [field_field] = "field",
  [field_function] = "function",
  [field_initializer] = "initializer",
  [field_left] = "left",
  [field_name] = "name",
  [field_object] = "object",
  [field_operator] = "operator",
  [field_parameters] = "parameters",
  [field_return_type] = "return_type",
  [field_right] = "right",
  [field_type] = "type",
  [field_value] = "value",
};

static const TSFieldMapSlice ts_field_map_slices[PRODUCTION_ID_COUNT] = {
  [1] = {.index = 0, .length = 3},
  [2] = {.index = 3, .length = 1},
  [3] = {.index = 4, .length = 2},
  [4] = {.index = 6, .length = 2},
  [5] = {.index = 8, .length = 4},
  [6] = {.index = 12, .length = 1},
  [7] = {.index = 13, .length = 1},
  [8] = {.index = 14, .length = 1},
  [9] = {.index = 15, .length = 3},
  [10] = {.index = 18, .length = 2},
  [11] = {.index = 20, .length = 3},
};

static const TSFieldMapEntry ts_field_map_entries[] = {
  [0] =
    {field_body, 3},
    {field_name, 1},
    {field_parameters, 2},
  [3] =
    {field_base_type, 0},
  [4] =
    {field_name, 0},
    {field_type, 2},
  [6] =
    {field_base_type, 0},
    {field_domain_tag, 1},
  [8] =
    {field_body, 5},
    {field_name, 1},
    {field_parameters, 2},
    {field_return_type, 4},
  [12] =
    {field_name, 1},
  [13] =
    {field_value, 1},
  [14] =
    {field_function, 0},
  [15] =
    {field_left, 0},
    {field_operator, 1},
    {field_right, 2},
  [18] =
    {field_field, 2},
    {field_object, 0},
  [20] =
    {field_initializer, 5},
    {field_name, 1},
    {field_type, 3},
};

static const TSSymbol ts_alias_sequences[PRODUCTION_ID_COUNT][MAX_ALIAS_SEQUENCE_LENGTH] = {
  [0] = {0},
};

static const uint16_t ts_non_terminal_alias_map[] = {
  0,
};

static const TSStateId ts_primary_state_ids[STATE_COUNT] = {
  [0] = 0,
  [1] = 1,
  [2] = 2,
  [3] = 3,
  [4] = 4,
  [5] = 5,
  [6] = 6,
  [7] = 7,
  [8] = 8,
  [9] = 9,
  [10] = 10,
  [11] = 11,
  [12] = 12,
  [13] = 13,
  [14] = 14,
  [15] = 15,
  [16] = 16,
  [17] = 17,
  [18] = 18,
  [19] = 19,
  [20] = 20,
  [21] = 21,
  [22] = 22,
  [23] = 23,
  [24] = 24,
  [25] = 25,
  [26] = 26,
  [27] = 27,
  [28] = 28,
  [29] = 29,
  [30] = 30,
  [31] = 31,
  [32] = 32,
  [33] = 33,
  [34] = 34,
  [35] = 35,
  [36] = 36,
  [37] = 37,
  [38] = 38,
  [39] = 39,
  [40] = 40,
  [41] = 41,
  [42] = 42,
  [43] = 43,
  [44] = 44,
  [45] = 45,
  [46] = 46,
  [47] = 47,
  [48] = 48,
};

static bool ts_lex(TSLexer *lexer, TSStateId state) {
  START_LEXER();
  eof = lexer->eof(lexer);
  switch (state) {
    case 0:
      if (eof) ADVANCE(12);
      ADVANCE_MAP(
        '(', 15,
        ')', 17,
        '+', 26,
        ',', 16,
        '-', 3,
        '.', 27,
        '/', 2,
        ':', 18,
        ';', 24,
        '=', 23,
        '@', 19,
        'f', 6,
        'l', 4,
        'r', 5,
        '{', 20,
        '}', 21,
      );
      if (('\t' <= lookahead && lookahead <= '\r') ||
          lookahead == ' ') SKIP(0);
      END_STATE();
    case 1:
      if (lookahead == ')') ADVANCE(17);
      if (lookahead == '/') ADVANCE(2);
      if (('\t' <= lookahead && lookahead <= '\r') ||
          lookahead == ' ') SKIP(1);
      if (('A' <= lookahead && lookahead <= 'Z') ||
          lookahead == '_' ||
          ('a' <= lookahead && lookahead <= 'z')) ADVANCE(28);
      END_STATE();
    case 2:
      if (lookahead == '/') ADVANCE(29);
      END_STATE();
    case 3:
      if (lookahead == '>') ADVANCE(14);
      END_STATE();
    case 4:
      if (lookahead == 'e') ADVANCE(9);
      END_STATE();
    case 5:
      if (lookahead == 'e') ADVANCE(10);
      END_STATE();
    case 6:
      if (lookahead == 'n') ADVANCE(13);
      END_STATE();
    case 7:
      if (lookahead == 'n') ADVANCE(25);
      END_STATE();
    case 8:
      if (lookahead == 'r') ADVANCE(7);
      END_STATE();
    case 9:
      if (lookahead == 't') ADVANCE(22);
      END_STATE();
    case 10:
      if (lookahead == 't') ADVANCE(11);
      END_STATE();
    case 11:
      if (lookahead == 'u') ADVANCE(8);
      END_STATE();
    case 12:
      ACCEPT_TOKEN(ts_builtin_sym_end);
      END_STATE();
    case 13:
      ACCEPT_TOKEN(anon_sym_fn);
      END_STATE();
    case 14:
      ACCEPT_TOKEN(anon_sym_DASH_GT);
      END_STATE();
    case 15:
      ACCEPT_TOKEN(anon_sym_LPAREN);
      END_STATE();
    case 16:
      ACCEPT_TOKEN(anon_sym_COMMA);
      END_STATE();
    case 17:
      ACCEPT_TOKEN(anon_sym_RPAREN);
      END_STATE();
    case 18:
      ACCEPT_TOKEN(anon_sym_COLON);
      END_STATE();
    case 19:
      ACCEPT_TOKEN(anon_sym_AT);
      END_STATE();
    case 20:
      ACCEPT_TOKEN(anon_sym_LBRACE);
      END_STATE();
    case 21:
      ACCEPT_TOKEN(anon_sym_RBRACE);
      END_STATE();
    case 22:
      ACCEPT_TOKEN(anon_sym_let);
      END_STATE();
    case 23:
      ACCEPT_TOKEN(anon_sym_EQ);
      END_STATE();
    case 24:
      ACCEPT_TOKEN(anon_sym_SEMI);
      END_STATE();
    case 25:
      ACCEPT_TOKEN(anon_sym_return);
      END_STATE();
    case 26:
      ACCEPT_TOKEN(anon_sym_PLUS);
      END_STATE();
    case 27:
      ACCEPT_TOKEN(anon_sym_DOT);
      END_STATE();
    case 28:
      ACCEPT_TOKEN(sym_identifier);
      if (('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'Z') ||
          lookahead == '_' ||
          ('a' <= lookahead && lookahead <= 'z')) ADVANCE(28);
      END_STATE();
    case 29:
      ACCEPT_TOKEN(sym_line_comment);
      if (lookahead != 0 &&
          lookahead != '\n') ADVANCE(29);
      END_STATE();
    default:
      return false;
  }
}

static const TSLexMode ts_lex_modes[STATE_COUNT] = {
  [0] = {.lex_state = 0},
  [1] = {.lex_state = 0},
  [2] = {.lex_state = 0},
  [3] = {.lex_state = 0},
  [4] = {.lex_state = 0},
  [5] = {.lex_state = 0},
  [6] = {.lex_state = 1},
  [7] = {.lex_state = 1},
  [8] = {.lex_state = 1},
  [9] = {.lex_state = 0},
  [10] = {.lex_state = 0},
  [11] = {.lex_state = 0},
  [12] = {.lex_state = 0},
  [13] = {.lex_state = 0},
  [14] = {.lex_state = 0},
  [15] = {.lex_state = 0},
  [16] = {.lex_state = 0},
  [17] = {.lex_state = 0},
  [18] = {.lex_state = 0},
  [19] = {.lex_state = 1},
  [20] = {.lex_state = 0},
  [21] = {.lex_state = 0},
  [22] = {.lex_state = 0},
  [23] = {.lex_state = 0},
  [24] = {.lex_state = 0},
  [25] = {.lex_state = 1},
  [26] = {.lex_state = 0},
  [27] = {.lex_state = 0},
  [28] = {.lex_state = 0},
  [29] = {.lex_state = 1},
  [30] = {.lex_state = 0},
  [31] = {.lex_state = 0},
  [32] = {.lex_state = 0},
  [33] = {.lex_state = 0},
  [34] = {.lex_state = 0},
  [35] = {.lex_state = 0},
  [36] = {.lex_state = 0},
  [37] = {.lex_state = 1},
  [38] = {.lex_state = 1},
  [39] = {.lex_state = 0},
  [40] = {.lex_state = 0},
  [41] = {.lex_state = 1},
  [42] = {.lex_state = 0},
  [43] = {.lex_state = 0},
  [44] = {.lex_state = 1},
  [45] = {.lex_state = 0},
  [46] = {.lex_state = 0},
  [47] = {.lex_state = 1},
  [48] = {.lex_state = 1},
};

static const uint16_t ts_parse_table[LARGE_STATE_COUNT][SYMBOL_COUNT] = {
  [0] = {
    [ts_builtin_sym_end] = ACTIONS(1),
    [anon_sym_fn] = ACTIONS(1),
    [anon_sym_DASH_GT] = ACTIONS(1),
    [anon_sym_LPAREN] = ACTIONS(1),
    [anon_sym_COMMA] = ACTIONS(1),
    [anon_sym_RPAREN] = ACTIONS(1),
    [anon_sym_COLON] = ACTIONS(1),
    [anon_sym_AT] = ACTIONS(1),
    [anon_sym_LBRACE] = ACTIONS(1),
    [anon_sym_RBRACE] = ACTIONS(1),
    [anon_sym_let] = ACTIONS(1),
    [anon_sym_EQ] = ACTIONS(1),
    [anon_sym_SEMI] = ACTIONS(1),
    [anon_sym_return] = ACTIONS(1),
    [anon_sym_PLUS] = ACTIONS(1),
    [anon_sym_DOT] = ACTIONS(1),
    [sym_line_comment] = ACTIONS(3),
  },
  [1] = {
    [sym_source_file] = STATE(46),
    [sym_function_item] = STATE(12),
    [aux_sym_source_file_repeat1] = STATE(12),
    [anon_sym_fn] = ACTIONS(5),
    [sym_line_comment] = ACTIONS(3),
  },
};

static const uint16_t ts_small_parse_table[] = {
  [0] = 5,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(7), 1,
      anon_sym_RBRACE,
    ACTIONS(9), 1,
      anon_sym_let,
    ACTIONS(11), 1,
      anon_sym_return,
    STATE(4), 4,
      sym__statement,
      sym_let_declaration,
      sym_return_statement,
      aux_sym_block_repeat1,
  [19] = 5,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(9), 1,
      anon_sym_let,
    ACTIONS(11), 1,
      anon_sym_return,
    ACTIONS(13), 1,
      anon_sym_RBRACE,
    STATE(2), 4,
      sym__statement,
      sym_let_declaration,
      sym_return_statement,
      aux_sym_block_repeat1,
  [38] = 5,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(15), 1,
      anon_sym_RBRACE,
    ACTIONS(17), 1,
      anon_sym_let,
    ACTIONS(20), 1,
      anon_sym_return,
    STATE(4), 4,
      sym__statement,
      sym_let_declaration,
      sym_return_statement,
      aux_sym_block_repeat1,
  [57] = 4,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(25), 1,
      anon_sym_AT,
    STATE(10), 1,
      sym_domain_tag,
    ACTIONS(23), 4,
      anon_sym_COMMA,
      anon_sym_RPAREN,
      anon_sym_LBRACE,
      anon_sym_EQ,
  [73] = 3,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(27), 1,
      sym_identifier,
    STATE(11), 4,
      sym__expression,
      sym_binary_expression,
      sym_field_expression,
      sym_call_expression,
  [86] = 3,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(29), 1,
      sym_identifier,
    STATE(16), 4,
      sym__expression,
      sym_binary_expression,
      sym_field_expression,
      sym_call_expression,
  [99] = 3,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(31), 1,
      sym_identifier,
    STATE(17), 4,
      sym__expression,
      sym_binary_expression,
      sym_field_expression,
      sym_call_expression,
  [112] = 4,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(33), 1,
      ts_builtin_sym_end,
    ACTIONS(35), 1,
      anon_sym_fn,
    STATE(9), 2,
      sym_function_item,
      aux_sym_source_file_repeat1,
  [126] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(38), 4,
      anon_sym_COMMA,
      anon_sym_RPAREN,
      anon_sym_LBRACE,
      anon_sym_EQ,
  [136] = 5,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(40), 1,
      anon_sym_LPAREN,
    ACTIONS(42), 1,
      anon_sym_SEMI,
    ACTIONS(44), 1,
      anon_sym_PLUS,
    ACTIONS(46), 1,
      anon_sym_DOT,
  [152] = 4,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(5), 1,
      anon_sym_fn,
    ACTIONS(48), 1,
      ts_builtin_sym_end,
    STATE(9), 2,
      sym_function_item,
      aux_sym_source_file_repeat1,
  [166] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(50), 4,
      anon_sym_LPAREN,
      anon_sym_SEMI,
      anon_sym_PLUS,
      anon_sym_DOT,
  [176] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(52), 4,
      anon_sym_COMMA,
      anon_sym_RPAREN,
      anon_sym_LBRACE,
      anon_sym_EQ,
  [186] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(54), 4,
      anon_sym_LPAREN,
      anon_sym_SEMI,
      anon_sym_PLUS,
      anon_sym_DOT,
  [196] = 4,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(40), 1,
      anon_sym_LPAREN,
    ACTIONS(46), 1,
      anon_sym_DOT,
    ACTIONS(56), 2,
      anon_sym_SEMI,
      anon_sym_PLUS,
  [210] = 5,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(40), 1,
      anon_sym_LPAREN,
    ACTIONS(44), 1,
      anon_sym_PLUS,
    ACTIONS(46), 1,
      anon_sym_DOT,
    ACTIONS(58), 1,
      anon_sym_SEMI,
  [226] = 4,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(60), 1,
      anon_sym_DASH_GT,
    ACTIONS(62), 1,
      anon_sym_LBRACE,
    STATE(33), 1,
      sym_block,
  [239] = 4,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(64), 1,
      anon_sym_RPAREN,
    ACTIONS(66), 1,
      sym_identifier,
    STATE(22), 1,
      sym_parameter,
  [252] = 4,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(68), 1,
      anon_sym_COMMA,
    ACTIONS(70), 1,
      anon_sym_RPAREN,
    STATE(23), 1,
      aux_sym_parameters_repeat1,
  [265] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(72), 3,
      anon_sym_RBRACE,
      anon_sym_let,
      anon_sym_return,
  [274] = 4,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(68), 1,
      anon_sym_COMMA,
    ACTIONS(74), 1,
      anon_sym_RPAREN,
    STATE(20), 1,
      aux_sym_parameters_repeat1,
  [287] = 4,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(76), 1,
      anon_sym_COMMA,
    ACTIONS(79), 1,
      anon_sym_RPAREN,
    STATE(23), 1,
      aux_sym_parameters_repeat1,
  [300] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(81), 3,
      anon_sym_RBRACE,
      anon_sym_let,
      anon_sym_return,
  [309] = 3,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(83), 1,
      sym_identifier,
    STATE(42), 1,
      sym_type,
  [319] = 3,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(85), 1,
      anon_sym_LPAREN,
    STATE(18), 1,
      sym_parameters,
  [329] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(87), 2,
      anon_sym_DASH_GT,
      anon_sym_LBRACE,
  [337] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(89), 2,
      ts_builtin_sym_end,
      anon_sym_fn,
  [345] = 3,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(83), 1,
      sym_identifier,
    STATE(31), 1,
      sym_type,
  [355] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(91), 2,
      anon_sym_DASH_GT,
      anon_sym_LBRACE,
  [363] = 3,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(62), 1,
      anon_sym_LBRACE,
    STATE(39), 1,
      sym_block,
  [373] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(93), 2,
      ts_builtin_sym_end,
      anon_sym_fn,
  [381] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(95), 2,
      ts_builtin_sym_end,
      anon_sym_fn,
  [389] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(97), 2,
      anon_sym_COMMA,
      anon_sym_RPAREN,
  [397] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(79), 2,
      anon_sym_COMMA,
      anon_sym_RPAREN,
  [405] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(99), 2,
      anon_sym_DASH_GT,
      anon_sym_LBRACE,
  [413] = 3,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(83), 1,
      sym_identifier,
    STATE(34), 1,
      sym_type,
  [423] = 3,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(66), 1,
      sym_identifier,
    STATE(35), 1,
      sym_parameter,
  [433] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(101), 2,
      ts_builtin_sym_end,
      anon_sym_fn,
  [441] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(103), 1,
      anon_sym_RPAREN,
  [448] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(105), 1,
      sym_identifier,
  [455] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(107), 1,
      anon_sym_EQ,
  [462] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(109), 1,
      anon_sym_COLON,
  [469] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(111), 1,
      sym_identifier,
  [476] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(113), 1,
      anon_sym_COLON,
  [483] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(115), 1,
      ts_builtin_sym_end,
  [490] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(117), 1,
      sym_identifier,
  [497] = 2,
    ACTIONS(3), 1,
      sym_line_comment,
    ACTIONS(119), 1,
      sym_identifier,
};

static const uint32_t ts_small_parse_table_map[] = {
  [SMALL_STATE(2)] = 0,
  [SMALL_STATE(3)] = 19,
  [SMALL_STATE(4)] = 38,
  [SMALL_STATE(5)] = 57,
  [SMALL_STATE(6)] = 73,
  [SMALL_STATE(7)] = 86,
  [SMALL_STATE(8)] = 99,
  [SMALL_STATE(9)] = 112,
  [SMALL_STATE(10)] = 126,
  [SMALL_STATE(11)] = 136,
  [SMALL_STATE(12)] = 152,
  [SMALL_STATE(13)] = 166,
  [SMALL_STATE(14)] = 176,
  [SMALL_STATE(15)] = 186,
  [SMALL_STATE(16)] = 196,
  [SMALL_STATE(17)] = 210,
  [SMALL_STATE(18)] = 226,
  [SMALL_STATE(19)] = 239,
  [SMALL_STATE(20)] = 252,
  [SMALL_STATE(21)] = 265,
  [SMALL_STATE(22)] = 274,
  [SMALL_STATE(23)] = 287,
  [SMALL_STATE(24)] = 300,
  [SMALL_STATE(25)] = 309,
  [SMALL_STATE(26)] = 319,
  [SMALL_STATE(27)] = 329,
  [SMALL_STATE(28)] = 337,
  [SMALL_STATE(29)] = 345,
  [SMALL_STATE(30)] = 355,
  [SMALL_STATE(31)] = 363,
  [SMALL_STATE(32)] = 373,
  [SMALL_STATE(33)] = 381,
  [SMALL_STATE(34)] = 389,
  [SMALL_STATE(35)] = 397,
  [SMALL_STATE(36)] = 405,
  [SMALL_STATE(37)] = 413,
  [SMALL_STATE(38)] = 423,
  [SMALL_STATE(39)] = 433,
  [SMALL_STATE(40)] = 441,
  [SMALL_STATE(41)] = 448,
  [SMALL_STATE(42)] = 455,
  [SMALL_STATE(43)] = 462,
  [SMALL_STATE(44)] = 469,
  [SMALL_STATE(45)] = 476,
  [SMALL_STATE(46)] = 483,
  [SMALL_STATE(47)] = 490,
  [SMALL_STATE(48)] = 497,
};

static const TSParseActionEntry ts_parse_actions[] = {
  [0] = {.entry = {.count = 0, .reusable = false}},
  [1] = {.entry = {.count = 1, .reusable = false}}, RECOVER(),
  [3] = {.entry = {.count = 1, .reusable = true}}, SHIFT_EXTRA(),
  [5] = {.entry = {.count = 1, .reusable = true}}, SHIFT(44),
  [7] = {.entry = {.count = 1, .reusable = true}}, SHIFT(28),
  [9] = {.entry = {.count = 1, .reusable = true}}, SHIFT(47),
  [11] = {.entry = {.count = 1, .reusable = true}}, SHIFT(6),
  [13] = {.entry = {.count = 1, .reusable = true}}, SHIFT(32),
  [15] = {.entry = {.count = 1, .reusable = true}}, REDUCE(aux_sym_block_repeat1, 2, 0, 0),
  [17] = {.entry = {.count = 2, .reusable = true}}, REDUCE(aux_sym_block_repeat1, 2, 0, 0), SHIFT_REPEAT(47),
  [20] = {.entry = {.count = 2, .reusable = true}}, REDUCE(aux_sym_block_repeat1, 2, 0, 0), SHIFT_REPEAT(6),
  [23] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_type, 1, 0, 2),
  [25] = {.entry = {.count = 1, .reusable = true}}, SHIFT(48),
  [27] = {.entry = {.count = 1, .reusable = true}}, SHIFT(11),
  [29] = {.entry = {.count = 1, .reusable = true}}, SHIFT(16),
  [31] = {.entry = {.count = 1, .reusable = true}}, SHIFT(17),
  [33] = {.entry = {.count = 1, .reusable = true}}, REDUCE(aux_sym_source_file_repeat1, 2, 0, 0),
  [35] = {.entry = {.count = 2, .reusable = true}}, REDUCE(aux_sym_source_file_repeat1, 2, 0, 0), SHIFT_REPEAT(44),
  [38] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_type, 2, 0, 4),
  [40] = {.entry = {.count = 1, .reusable = true}}, SHIFT(40),
  [42] = {.entry = {.count = 1, .reusable = true}}, SHIFT(24),
  [44] = {.entry = {.count = 1, .reusable = true}}, SHIFT(7),
  [46] = {.entry = {.count = 1, .reusable = true}}, SHIFT(41),
  [48] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_source_file, 1, 0, 0),
  [50] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_field_expression, 3, 0, 10),
  [52] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_domain_tag, 2, 0, 6),
  [54] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_call_expression, 3, 0, 8),
  [56] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_binary_expression, 3, 0, 9),
  [58] = {.entry = {.count = 1, .reusable = true}}, SHIFT(21),
  [60] = {.entry = {.count = 1, .reusable = true}}, SHIFT(29),
  [62] = {.entry = {.count = 1, .reusable = true}}, SHIFT(3),
  [64] = {.entry = {.count = 1, .reusable = true}}, SHIFT(30),
  [66] = {.entry = {.count = 1, .reusable = true}}, SHIFT(43),
  [68] = {.entry = {.count = 1, .reusable = true}}, SHIFT(38),
  [70] = {.entry = {.count = 1, .reusable = true}}, SHIFT(36),
  [72] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_let_declaration, 7, 0, 11),
  [74] = {.entry = {.count = 1, .reusable = true}}, SHIFT(27),
  [76] = {.entry = {.count = 2, .reusable = true}}, REDUCE(aux_sym_parameters_repeat1, 2, 0, 0), SHIFT_REPEAT(38),
  [79] = {.entry = {.count = 1, .reusable = true}}, REDUCE(aux_sym_parameters_repeat1, 2, 0, 0),
  [81] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_return_statement, 3, 0, 7),
  [83] = {.entry = {.count = 1, .reusable = true}}, SHIFT(5),
  [85] = {.entry = {.count = 1, .reusable = true}}, SHIFT(19),
  [87] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_parameters, 3, 0, 0),
  [89] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_block, 3, 0, 0),
  [91] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_parameters, 2, 0, 0),
  [93] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_block, 2, 0, 0),
  [95] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_function_item, 4, 0, 1),
  [97] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_parameter, 3, 0, 3),
  [99] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_parameters, 4, 0, 0),
  [101] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_function_item, 6, 0, 5),
  [103] = {.entry = {.count = 1, .reusable = true}}, SHIFT(15),
  [105] = {.entry = {.count = 1, .reusable = true}}, SHIFT(13),
  [107] = {.entry = {.count = 1, .reusable = true}}, SHIFT(8),
  [109] = {.entry = {.count = 1, .reusable = true}}, SHIFT(37),
  [111] = {.entry = {.count = 1, .reusable = true}}, SHIFT(26),
  [113] = {.entry = {.count = 1, .reusable = true}}, SHIFT(25),
  [115] = {.entry = {.count = 1, .reusable = true}},  ACCEPT_INPUT(),
  [117] = {.entry = {.count = 1, .reusable = true}}, SHIFT(45),
  [119] = {.entry = {.count = 1, .reusable = true}}, SHIFT(14),
};

#ifdef __cplusplus
extern "C" {
#endif
#ifdef TREE_SITTER_HIDE_SYMBOLS
#define TS_PUBLIC
#elif defined(_WIN32)
#define TS_PUBLIC __declspec(dllexport)
#else
#define TS_PUBLIC __attribute__((visibility("default")))
#endif

TS_PUBLIC const TSLanguage *tree_sitter_myhdl(void) {
  static const TSLanguage language = {
    .version = LANGUAGE_VERSION,
    .symbol_count = SYMBOL_COUNT,
    .alias_count = ALIAS_COUNT,
    .token_count = TOKEN_COUNT,
    .external_token_count = EXTERNAL_TOKEN_COUNT,
    .state_count = STATE_COUNT,
    .large_state_count = LARGE_STATE_COUNT,
    .production_id_count = PRODUCTION_ID_COUNT,
    .field_count = FIELD_COUNT,
    .max_alias_sequence_length = MAX_ALIAS_SEQUENCE_LENGTH,
    .parse_table = &ts_parse_table[0][0],
    .small_parse_table = ts_small_parse_table,
    .small_parse_table_map = ts_small_parse_table_map,
    .parse_actions = ts_parse_actions,
    .symbol_names = ts_symbol_names,
    .field_names = ts_field_names,
    .field_map_slices = ts_field_map_slices,
    .field_map_entries = ts_field_map_entries,
    .symbol_metadata = ts_symbol_metadata,
    .public_symbol_map = ts_symbol_map,
    .alias_map = ts_non_terminal_alias_map,
    .alias_sequences = &ts_alias_sequences[0][0],
    .lex_modes = ts_lex_modes,
    .lex_fn = ts_lex,
    .primary_state_ids = ts_primary_state_ids,
  };
  return &language;
}
#ifdef __cplusplus
}
#endif
