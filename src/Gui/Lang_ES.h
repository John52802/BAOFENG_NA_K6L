#ifndef __LANG_ES_H
    #define __LANG_ES_H

extern Boolean Lang_IsGlyph(U8 ch);
extern void Lang_Glyph(U8 ch, U8 fontSize, U8 *dest);
extern const U8 *Lang_Translate(const U8 *src, U8 *dest, U8 destSize);

#endif
