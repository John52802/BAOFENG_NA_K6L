/* Spanish on-screen text. Generated once from the radio's own fonts; edit by hand. */
#include "includes.h"

/* Characters drawn from Lang_Glyph instead of the flash font. */
#define GLYPH_FIRST 0x80
#define GLYPH_LAST  0x86

static const U8 glyph8x16[][16] =
{
    {0x20,0x90,0x90,0x98,0x94,0x12,0xE0,0x00,0x07,0x08,0x08,0x08,0x08,0x05,0x0F,0x00}, /* a acute */
    {0xC0,0xA0,0x90,0x98,0x94,0xA2,0xC0,0x00,0x03,0x04,0x08,0x08,0x08,0x08,0x04,0x00}, /* e acute */
    {0x00,0x00,0x00,0xF8,0x04,0x02,0x00,0x00,0x00,0x00,0x00,0x0F,0x00,0x00,0x00,0x00}, /* i acute */
    {0xC0,0x20,0x10,0x18,0x14,0x22,0xC0,0x00,0x03,0x04,0x08,0x08,0x08,0x04,0x03,0x00}, /* o acute */
    {0x00,0xF0,0x00,0x08,0x04,0x02,0xF0,0x00,0x00,0x07,0x08,0x08,0x08,0x04,0x0F,0x00}, /* u acute */
    {0x00,0xF4,0x22,0x12,0x14,0x14,0xE2,0x00,0x00,0x0F,0x00,0x00,0x00,0x00,0x0F,0x00}, /* n tilde */
    {0x00,0x00,0x38,0x78,0x40,0x80,0x00,0x00,0x06,0x08,0x10,0x10,0x10,0x08,0x07,0x00}, /* inverted question mark */
};

static const U8 glyph6x12[][12] =
{
    {0xC8,0x28,0x2A,0xF1,0x00,0x00,0x00,0x01,0x01,0x00,0x01,0x00}, /* a acute */
    {0xF0,0x28,0x2A,0x29,0xB0,0x00,0x00,0x01,0x01,0x01,0x00,0x00}, /* e acute */
    {0x00,0x00,0xFA,0x01,0x00,0x00,0x00,0x00,0x01,0x00,0x00,0x00}, /* i acute */
    {0xF0,0x08,0x0A,0xF1,0x00,0x00,0x00,0x01,0x01,0x00,0x00,0x00}, /* o acute */
    {0xF8,0x00,0x82,0xF9,0x80,0x00,0x00,0x01,0x00,0x01,0x00,0x00}, /* u acute */
    {0xF2,0x09,0x0A,0xF1,0x00,0x00,0x01,0x00,0x00,0x01,0x00,0x00}, /* n tilde */
    {0x80,0x00,0x18,0x60,0x80,0x00,0x01,0x02,0x02,0x02,0x01,0x00}, /* inverted question mark */
};

typedef struct
{
    const char *en;
    const char *es;
} STR_LANG_ITEM;

/* Keys are the English text as shown: menu titles are cut to 12 characters.
   Text not listed here stays in English. In these strings, octal escapes are:
   200 a, 201 e, 202 i, 203 o, 204 u (accented), 205 n with tilde, 206 inverted question mark. */
static const STR_LANG_ITEM langTable[] =
{
    {"OFF", "Apagado"},
    {"ON", "Encendido"},
    {"ON SITE", "Local"},
    {"SEND SOUND", "Enviar sonido"},
    {"SEND CODE", "Enviar c\203digo"},
    {"BOT", "Inicio"},
    {"EOT", "Final"},
    {"BOTH", "Ambos"},
    {"WIDE", "Ancha"},
    {"NARROW", "Estrecha"},
    {"HIGH", "Alta"},
    {"LOW", "Baja"},
    {"NAME", "Nombre"},
    {"FREQ", "Frecuencia"},
    {"CH", "Canal"},
    {"NAME+FREQ", "Nombre+Frec"},
    {"All", "Todo"},
    {"MESSAGE", "Mensaje"},
    {"VOLTAGE", "Voltaje"},
    {"None", "Ninguno"},
    {"Torch On/Off", "Linterna"},
    {"Power Select", "Potencia"},
    {"Scan On/Off", "Escaneo"},
    {"VOX On/Off", "VOX"},
    {"Alarm on/off", "Alarma"},
    {"Radio on/off", "Radio FM"},
    {"BEEP", "Pitido"},
    {"Reflex", "Inverso"},
    {"Tone", "Tono"},
    {"Voice", "Voz"},
    {"CTDCS+Signaling", "CTDCS+Se\205al"},
    {"Double Wait", "Doble"},
    {"Signal Wait", "Por se\205al"},
    {"out of range!", "Fuera de rango"},
    {"Wait...", "Espere..."},
    {"Sure to Reset?", "\206Reiniciar?"},
    {"Please Wait...", "Espere..."},
    {"Input Password", "Escribe clave"},
    {"Password Error", "Clave err\203nea"},
    {"Program...", "Programando"},
    {"SEEK...", "Buscando"},
    {"No Name", "Sin nombre"},
    {"SCANQT", "Buscar tono"},
    {"STOP", "Detenido"},
    {"RUNNING", "En curso"},
    {"NONE", "Ninguno"},
    {"SEARCH", "Buscar"},
    {"CHL Name", "Nombre canal"},
    {"RX Frequency", "Frec. RX"},
    {"TX Frequency", "Frec. TX"},
    {"R-CTCSS", "RX CTCSS"},
    {"R-DCS", "RX DCS"},
    {"T-CTCSS", "TX CTCSS"},
    {"T-DCS", "TX DCS"},
    {"BandWidth", "Ancho banda"},
    {"TXP", "Potencia TX"},
    {"Silence Mode", "Silenciado"},
    {"TX Forbid", "Prohibir TX"},
    {"Shift DIR", "Dir. despl."},
    {"OFFSET", "Desplazam."},
    {"MEMCH", "Guardar CH"},
    {"DELCH", "Borrar CH"},
    {"VFO FreqRang", "Rango VFO"},
    {"Scan Mode", "Modo escaneo"},
    {"DTMFST", "Tono DTMF"},
    {"PTT-LT", "Retardo ID"},
    {"Squelch", "Silenciador"},
    {"RX Save", "Ahorro RX"},
    {"VOX Switch", "VOX"},
    {"VOX Level", "Nivel VOX"},
    {"VOX Delay", "Retardo VOX"},
    {"TX OVer Time", "Tiempo TX"},
    {"Language", "Idioma"},
    {"VOICE", "Voz"},
    {"Menu HangTim", "Tiempo men\204"},
    {"BEEP PROMPT", "Pitido"},
    {"Roger Beep", "Tono fin TX"},
    {"POWER ON TYP", "Tipo inicio"},
    {"Power On Ton", "Tono inicio"},
    {"Power on MSG", "Msj. inicio"},
    {"POWER ON PWD", "Clave inicio"},
    {"Dual Watch", "Doble escuch"},
    {"MDF-A", "Vista A"},
    {"MDF-B", "Vista B"},
    {"ALERT", "Alerta"},
    {"Freq Step", "Paso frec."},
    {"Busy Lockout", "Bloq ocupado"},
    {"Side Tone", "Tono local"},
    {"Alarm Mode", "Modo alarma"},
    {"PF1 LONG PRE", "PF1 largo"},
    {"ABR", "Luz pantalla"},
    {"Brightness", "Brillo"},
    {"LCD Reflex", "LCD inverso"},
    {"AUTOLOCK", "Autobloqueo"},
    {"Radio Interr", "Interr radio"},
    {"Reset", "Reiniciar"},
    {"STOP WATCH", "Cron\203metro"},
    {"VERSION", "Versi\203n"},
    {"FM MEMCH", "Guardar FM"},
    {"FM BAND", "Banda FM"},
};

extern Boolean Lang_IsGlyph(U8 ch)
{
    return (ch >= GLYPH_FIRST && ch <= GLYPH_LAST) ? TRUE : FALSE;
}

extern void Lang_Glyph(U8 ch, U8 fontSize, U8 *dest)
{
    if(fontSize == FONTSIZE_16x16)
    {
        memcpy(dest, glyph8x16[ch - GLYPH_FIRST], 16);
    }
    else
    {
        memcpy(dest, glyph6x12[ch - GLYPH_FIRST], 12);
    }
}

/* Returns the text to draw: src itself, or dest holding the translation,
   centred in the same width as the original padded text. */
extern const U8 *Lang_Translate(const U8 *src, U8 *dest, U8 destSize)
{
    U16 total, start, len, newLen, pad, i;

    if(g_radioInform.language != LANG_ES)
    {
        return src;
    }

    total = strlen((const char *)src);
    start = 0;
    while(start < total && src[start] == ' ')
    {
        start++;
    }
    len = total - start;
    while(len > 0 && src[start + len - 1] == ' ')
    {
        len--;
    }
    if(len == 0)
    {
        return src;
    }

    for(i = 0; i < sizeof(langTable) / sizeof(langTable[0]); i++)
    {
        if(strlen(langTable[i].en) == len && memcmp(langTable[i].en, &src[start], len) == 0)
        {
            newLen = strlen(langTable[i].es);
            if(newLen + 1 > destSize)
            {
                return src;
            }
            pad = 0;
            if(total > newLen && total < destSize)
            {
                pad = (total - newLen) / 2;
            }
            else
            {
                total = newLen;
            }
            memset(dest, ' ', total);
            memcpy(&dest[pad], langTable[i].es, newLen);
            dest[total] = 0;
            return dest;
        }
    }
    return src;
}
