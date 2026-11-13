// Linote: minimal Lexilla catalogue exposing only the lexers bundled with the app.
// Replaces Lexilla.cxx (which references every lexer) so that only selected
// lexers are linked statically into the executable.
#include <cstring>
#include <string>
#include <string_view>
#include <map>
#include <initializer_list>
#include <vector>

#include "ILexer.h"
#include "Scintilla.h"
#include "SciLexer.h"
#include "PropSetSimple.h"
#include "LexerModule.h"
#include "CatalogueModules.h"

using namespace Lexilla;

extern LexerModule lmBatch;
extern LexerModule lmCPP;
extern LexerModule lmErrorList;
extern LexerModule lmHTML;
extern LexerModule lmJSON;
extern LexerModule lmMarkdown;
extern LexerModule lmNull;
extern LexerModule lmPowerShell;
extern LexerModule lmProps;
extern LexerModule lmPython;
extern LexerModule lmRust;
extern LexerModule lmSQL;
extern LexerModule lmXML;
extern LexerModule lmYAML;

namespace {
CatalogueModules catalogue;

void EnsureCatalogue() {
	if (catalogue.Count() == 0) {
		catalogue.AddLexerModules({
			&lmBatch, &lmCPP, &lmErrorList, &lmHTML, &lmJSON, &lmMarkdown, &lmNull,
			&lmPowerShell, &lmProps, &lmPython, &lmRust, &lmSQL, &lmXML, &lmYAML,
		});
	}
}
}

extern "C" Scintilla::ILexer5 *linote_create_lexer(const char *name) {
	EnsureCatalogue();
	for (size_t i = 0; i < catalogue.Count(); i++) {
		const char *lexerName = catalogue.Name(i);
		if (lexerName && name && 0 == strcmp(lexerName, name)) {
			return catalogue.Create(i);
		}
	}
	return nullptr;
}
