LATEXMK ?= latexmk

.PHONY: all clean
all: harwness-paper.pdf

harwness-paper.pdf: harwness-paper.tex body.tex endmatter.tex architecture.tex references.bib
	$(LATEXMK) -pdf -interaction=nonstopmode -halt-on-error -file-line-error harwness-paper.tex

clean:
	$(LATEXMK) -c harwness-paper.tex
