/*
 * win32u loads FreeType with dlopen and dlsym. In this module FreeType
 * (Emscripten's port) is linked in, so freetype.c is compiled with these
 * two in place of the real ones: the library "opens" and each symbol it
 * asks for comes from this table.
 */

#include <string.h>
#include <ft2build.h>
#include FT_FREETYPE_H
#include FT_GLYPH_H
#include FT_TRIGONOMETRY_H
#include FT_LCD_FILTER_H
#include FT_MODULE_H
#include FT_OUTLINE_H
#include FT_SFNT_NAMES_H
#include FT_TRUETYPE_TABLES_H
#include FT_TRUETYPE_TAGS_H
#include FT_WINFONTS_H

#define F(name) { #name, (void *)name }
static const struct { const char *name; void *ptr; } symbols[] =
{
    F(FT_Done_Face), F(FT_Get_Char_Index), F(FT_Get_First_Char), F(FT_Get_Next_Char), F(FT_Get_Sfnt_Name),
    F(FT_Get_Sfnt_Name_Count), F(FT_Get_Sfnt_Table), F(FT_Get_TrueType_Engine_Type), F(FT_Get_WinFNT_Header),
    F(FT_Init_FreeType), F(FT_Library_SetLcdFilter), F(FT_Library_Version), F(FT_Load_Glyph),
    F(FT_Load_Sfnt_Table), F(FT_Matrix_Multiply), F(FT_MulDiv), F(FT_MulFix), F(FT_New_Face),
    F(FT_New_Memory_Face), F(FT_Outline_Embolden), F(FT_Outline_Get_Bitmap), F(FT_Outline_Get_CBox),
    F(FT_Outline_Transform), F(FT_Outline_Translate), F(FT_Property_Set), F(FT_Render_Glyph),
    F(FT_Set_Charmap), F(FT_Set_Pixel_Sizes), F(FT_Vector_Length), F(FT_Vector_Transform), F(FT_Vector_Unit),
};

void *wasm_dlopen( const char *name, int flags )
{
    return name && strstr( name, "freetype" ) ? (void *)symbols : NULL;
}

void *wasm_dlsym( void *handle, const char *name )
{
    unsigned int i;
    if (handle != (void *)symbols) return NULL;
    for (i = 0; i < sizeof(symbols) / sizeof(symbols[0]); i++)
        if (!strcmp( symbols[i].name, name )) return symbols[i].ptr;
    return NULL;
}

char *wasm_dlerror( void )
{
    return (char *)"symbol not linked into the module";
}
