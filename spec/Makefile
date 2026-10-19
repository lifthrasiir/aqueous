PIKCHR ?= pikchr

index.html: index.bs figures/segments.svg
	pipx run bikeshed spec $< $@

# Generated SVGs are committed so that building index.html doesn't need pikchr.
# Colors are made themeable: black follows the text color and fills become CSS properties.
figures/%.svg: figures/%.pik
	$(PIKCHR) --svg-only $< > $@.tmp
	sed -e 's/rgb(0,0,0)/currentColor/g' \
	    -e 's/rgb(201,220,245)/var(--fig-leaf,&)/g' \
	    -e 's/rgb(232,232,232)/var(--fig-nonleaf,&)/g' \
	    -e 's/rgb(213,234,208)/var(--fig-aux,&)/g' \
	    -e 's/rgb(245,220,180)/var(--fig-meta,&)/g' \
	    -e 's/ data-pikchr-date="[0-9]*"//' \
	    $@.tmp > $@
	rm $@.tmp
