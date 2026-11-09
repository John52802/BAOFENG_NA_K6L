#include "includes.h"

/*
 * Receive-only spectrum view.
 *
 * Runs its own loop instead of the main task loop. PTT_ScanTask and RF_Task are
 * never called from here, so the radio cannot enter TX while this mode is open.
 *
 * Keys: UP/DOWN move the centre, * narrows the span, # widens it, MENU goes towards the
 * peak: it zooms in on it and, at the narrowest span, listens to it. EXIT leaves.
 * Listening tunes the receiver only. The VFO and channels aren't changed.
 * Frequencies are in units of 10 Hz, matching the rest of the firmware.
 */

extern U8 gLcdBuffer[8][128];
extern void Rfic_BandInitial(U32 freq);
extern Boolean Rfic_IsAmRx(U32 freq);
extern void Rfic_SetScramble(U8 group, U32 freq);

#define SP_COLS        128
#define SP_GRAPH_TOP   24
#define SP_GRAPH_H     40
#define SP_SETTLE_MS   25   /* ms: the level reading follows a signal slowly, measured on the radio */
/* REG_7E: bit 15 fixes the AGC, bits 14:12 choose its index. With automatic AGC the level
   reading climbs for over a second whatever the frequency, as measured on the radio.
   Index 010 is one step below maximum gain, so a nearby signal is less likely to saturate. */
#define SP_AGC_REG     0x7E
#define SP_AGC_MASK    0x0FFF
#define SP_AGC_FIXED   0xA000
#define SP_COLS_PER_POINT 2 /* Fewer points, so the longer wait keeps a sweep near 2 seconds */
#define SP_START_WAIT  800  /* ms: the level reading restarts when the receiver is switched on */
#define SP_MIN_RANGE   20
#define SP_REDRAW_COLS 16   /* A new span draws as it goes, so a key press shows at once */
#define SP_RX_MIN      (RX_MIN_100KHZ * 10000UL)
#define SP_RX_MAX      (RX_MAX_100KHZ * 10000UL)

/* The level reading lags the tuning, so a sweep upwards shows a peak above the real signal:
   measured about 275 kHz off at 2 MHz of span, and 3 MHz off at 10 MHz. */
#define SP_ZOOM_SHIFT_DIV 5    /* Zooming in moves down this part of the span, towards the signal. */
#define SP_FIND_BELOW  45000UL /* The exact search covers 450 kHz below the peak... */
#define SP_FIND_ABOVE  10000UL /* ...and 100 kHz above it... */
#define SP_FIND_STEP   1250UL  /* ...in steps of 12.5 kHz. */
#define SP_FIND_WAIT   300     /* ms: lets the reading forget the frequency before. */
#define SP_LISTEN_STEP 500UL   /* The arrows move 5 kHz while listening. */
#define SP_BAR_LEFT    20
#define SP_BAR_WIDTH   88
#define SP_BAR_PAGE    5       /* Row of the LCD buffer: dots 40 to 47. */
#define SP_RSSI_LOW    35      /* The same scale as the bar of the main screen. */
#define SP_RSSI_HIGH   115

enum { SP_KEEP_GOING = 0, SP_RESTART, SP_EXIT, SP_PEAK };

static const U32 spSpanTab[] = { 200000UL, 500000UL, 1000000UL, 2000000UL, 5000000UL };
#define SP_SPAN_CNT    (sizeof(spSpanTab) / sizeof(spSpanTab[0]))

static U32 spCenter;
static U8  spSpanIdx;
static U8  spRssi[SP_COLS];
static U8  spLastBand;
static U32 spShownPeak;   /* The peak in the header, and whether there is a real one. */
static U8  spHasPeak;

static void SpClampCenter(void)
{
    U32 half = spSpanTab[spSpanIdx] / 2;

    if(spCenter < SP_RX_MIN + half)
    {
        spCenter = SP_RX_MIN + half;
    }
    else if(spCenter > SP_RX_MAX - half)
    {
        spCenter = SP_RX_MAX - half;
    }
}

static U8 SpHandleKey(U8 keyEvent)
{
    U32 quarter = spSpanTab[spSpanIdx] / 4;

    switch(keyEvent)
    {
        case KEYID_UP:
        case KEYID_UP_CONTINUE:
            spCenter += quarter;
            break;
        case KEYID_DOWN:
        case KEYID_DOWN_CONTINUE:
            spCenter -= quarter;
            break;
        case KEYID_STAR:
            if(spSpanIdx == 0)
            {
                return SP_KEEP_GOING;
            }
            spSpanIdx--;
            break;
        case KEYID_WELL:
            if(spSpanIdx >= SP_SPAN_CNT - 1)
            {
                return SP_KEEP_GOING;
            }
            spSpanIdx++;
            break;
        case KEYID_EXIT:
            return SP_EXIT;
        case KEYID_MENU:
            return SP_PEAK;
        default:
            return SP_KEEP_GOING;
    }

    SpClampCenter();
    return SP_RESTART;
}

/* Keeps keys, backlight and the power switch alive. */
static void SpServiceTasks(void)
{
    if(g_10msFlag)
    {
        g_10msFlag = FALSE;
        KEY_ScanTask();
    }
    if(g_50msFlag)
    {
        g_50msFlag = FALSE;
        LCD_CheckBackLight();
    }
    if(g_100msFlag)
    {
        g_100msFlag = FALSE;
        CheckPowerOff();
    }
    g_500msFlag = FALSE;
}

static U8 SpService(void)
{
    SpServiceTasks();
    if(g_keyScan.keyEvent != KEYID_NONE)
    {
        return SpHandleKey(Key_GetRealEvent());
    }
    return SP_KEEP_GOING;
}

static void SpTune(U32 freq)
{
    GetHardWorkBand(freq / 10000);
    if(g_ChannelVfoInfo.BandFlag != spLastBand)
    {
        spLastBand = g_ChannelVfoInfo.BandFlag;
        /* PWR_RXON only drives the RX path pins. */
        RF_PowerSet(spLastBand, PWR_RXON);
    }

    /* Measured on the radio: switching the receiver off and on leaves the level reading
       near zero for over 500 ms, so only the VCO calibration bit of REG_30 is pulsed. */
    Rfic_BandInitial(freq);
    Rfic_WriteWord(0x30, 0x3FF1);
    Rfic_WriteWord(0x30, 0xBFF1);
}

/* Draws the columns before `upto`. With none yet, only the header, without a peak. */
static void SpDraw(U8 upto, U8 peakCol, U8 minVal, U8 maxVal)
{
    String text[32];
    U32 span = spSpanTab[spSpanIdx];
    U32 start = spCenter - span / 2;
    U32 peak = start + (span / SP_COLS) * peakCol;
    /* A flat sweep has no peak: the first column would look like one. */
    spHasPeak = (upto != 0 && maxVal - minVal >= 2);
    spShownPeak = spHasPeak ? peak : spCenter;
    U16 range = maxVal - minVal;
    U8 col, y, height;

    if(range < SP_MIN_RANGE)
    {
        range = SP_MIN_RANGE;
    }

    LCD_ClearWorkArea();

    if(upto == 0)
    {
        sprintf(text, "%3lu.%03lu ~---.--- %2luM",
                spCenter / 100000UL, (spCenter % 100000UL) / 100UL,
                span / 100000UL);
    }
    else
    {
        sprintf(text, "%3lu.%03lu ~%3lu.%03lu %2luM",
                spCenter / 100000UL, (spCenter % 100000UL) / 100UL,
                peak / 100000UL, (peak % 100000UL) / 100UL,
                span / 100000UL);
    }
    LCD_DisplayText(9, 0, (U8 *)text, FONTSIZE_12x12, LCD_DIS_NORMAL);

    for(col = 0; col < upto; col++)
    {
        height = (U8)(((U16)(spRssi[col] - minVal) * SP_GRAPH_H) / range);
        for(y = SP_GRAPH_TOP + SP_GRAPH_H - height; y < SP_GRAPH_TOP + SP_GRAPH_H; y++)
        {
            gLcdBuffer[y / 8][col] |= (U8)(1 << (y % 8));
        }
    }

    /* Centre marker along the top edge of the graph. */
    gLcdBuffer[SP_GRAPH_TOP / 8][SP_COLS / 2] |= 0x07;

    /* A flat sweep has nothing to draw: show the raw reading instead of an empty graph. */
    if(upto == SP_COLS && maxVal - minVal < 2)
    {
        sprintf(text, "RSSI %u", maxVal);
        LCD_DisplayText(36, 40, (U8 *)text, FONTSIZE_12x12, LCD_DIS_NORMAL);
    }

    LCD_UpdateWorkAre();
}

static void SpShowText(U8 y, U8 x, String *text, U8 font)
{
    LCD_DisplayText(y, x, (U8 *)text, font, LCD_DIS_NORMAL);
}

/* A sweep up and a sweep down lag in opposite directions, so the signal is halfway between
   their peaks. Returns `approx` if neither sees anything standing out. */
static U32 SpFindExact(U32 approx)
{
    U32 low = approx - SP_FIND_BELOW;
    U32 high = approx + SP_FIND_ABOVE;
    U32 freq, best[2];
    U16 reading, top, bottom;
    U8  pass, found = TRUE;
    U16 i, points;

    if(low < SP_RX_MIN)
    {
        low = SP_RX_MIN;
    }
    if(high > SP_RX_MAX)
    {
        high = SP_RX_MAX;
    }
    points = (U16)((high - low) / SP_FIND_STEP) + 1;

    LCD_ClearWorkArea();
    SpShowText(28, 24, "BUSCANDO", FONTSIZE_16x16);
    LCD_UpdateWorkAre();

    for(pass = 0; pass < 2; pass++)
    {
        top = 0;
        bottom = 0xFFFF;
        best[pass] = approx;
        SpTune(pass ? high : low);
        DelayMs(SP_FIND_WAIT);
        for(i = 0; i < points; i++)
        {
            SpServiceTasks();
            freq = pass ? high - SP_FIND_STEP * i : low + SP_FIND_STEP * i;
            SpTune(freq);
            DelayMs(SP_SETTLE_MS);
            reading = Rfic_GetRssiVal();
            if(reading > top)
            {
                top = reading;
                best[pass] = freq;
            }
            if(reading < bottom)
            {
                bottom = reading;
            }
        }
        if(top - bottom < 2)
        {
            found = FALSE;
        }
    }
    if(!found)
    {
        return approx;
    }
    /* On the 5 kHz grid of the arrows. */
    freq = (best[0] + best[1]) / 2;
    return (freq + SP_LISTEN_STEP / 2) / SP_LISTEN_STEP * SP_LISTEN_STEP;
}

static void SpListenTune(U32 freq)
{
    /* The same choice normal reception makes: the air band and forced AM are AM. */
    Rfic_SwitchFM_AM(Rfic_IsAmRx(freq) ? ModeAM : ModeFM);
    SpTune(freq);
    Rfic_SetScramble(0, freq);
    Rfic_SetAfout(ON);
}

static void SpListenDraw(U32 freq)
{
    String text[16];

    LCD_ClearWorkArea();
    sprintf(text, "ESCUCHA %s", Rfic_IsAmRx(freq) ? "AM" : "FM");
    SpShowText(9, 0, text, FONTSIZE_12x12);
    sprintf(text, "%3lu.%03lu", freq / 100000UL, (freq % 100000UL) / 100UL);
    SpShowText(20, 36, text, FONTSIZE_16x16);
    SpShowText(50, 0, "FLECHAS AJUSTAN  EXIT", FONTSIZE_12x12);
}

static void SpListenBar(void)
{
    U16 rssi = Rfic_GetRssiVal();
    U8  x, width;

    if(rssi <= SP_RSSI_LOW)
    {
        width = 0;
    }
    else if(rssi >= SP_RSSI_HIGH)
    {
        width = SP_BAR_WIDTH - 2;
    }
    else
    {
        width = (U8)((rssi - SP_RSSI_LOW) * (SP_BAR_WIDTH - 2) / (SP_RSSI_HIGH - SP_RSSI_LOW));
    }
    for(x = 0; x < SP_BAR_WIDTH; x++)
    {
        gLcdBuffer[SP_BAR_PAGE][SP_BAR_LEFT + x] = (x == 0 || x == SP_BAR_WIDTH - 1 || x <= width) ? 0xFF : 0x81;
    }
    LCD_UpdateWorkAre();
}

/* Opens the audio on one frequency whatever the squelch and tones say, like monitor mode. */
static void SpListen(U32 freq, U16 normalAgc)
{
    U8 tick, key;

    Rfic_WriteWord(SP_AGC_REG, normalAgc);
    Rfic_SetToneFreq(0);
    Rfic_WriteWord(0x70, 0x0000);
    SpListenTune(freq);
    LedRxSwitch(LED_ON);
    SpeakerSwitch(ON);
    SpListenDraw(freq);
    SpListenBar();

    while(1)
    {
        tick = g_100msFlag;
        SpServiceTasks();
        if(tick)
        {
            SpListenBar();
        }
        if(g_keyScan.keyEvent == KEYID_NONE)
        {
            continue;
        }
        key = Key_GetRealEvent();
        if(key == KEYID_EXIT || key == KEYID_MENU)
        {
            break;
        }
        if(key == KEYID_UP || key == KEYID_UP_CONTINUE)
        {
            if(freq + SP_LISTEN_STEP >= SP_RX_MAX)
            {
                continue;
            }
            freq += SP_LISTEN_STEP;
        }
        else if(key == KEYID_DOWN || key == KEYID_DOWN_CONTINUE)
        {
            if(freq < SP_RX_MIN + SP_LISTEN_STEP)
            {
                continue;
            }
            freq -= SP_LISTEN_STEP;
        }
        else
        {
            continue;
        }
        SpListenTune(freq);
        SpListenDraw(freq);
        SpListenBar();
    }

    SpeakerSwitch(OFF);
    Rfic_SetAfout(OFF);
    LedRxSwitch(LED_OFF);
    Rfic_SwitchFM_AM(ModeFM);
    Rfic_WriteWord(SP_AGC_REG, (normalAgc & SP_AGC_MASK) | SP_AGC_FIXED);
    /* The next sweep starts around what was just heard. */
    spCenter = freq;
    SpClampCenter();
}

extern void EnterSpectrumMode(void)
{
    U8  col, action, peakCol, minVal, maxVal, fresh;
    U16 reading;
    U32 start, step;
    U16 savedAgc;

    DualStandbyWorkOFF();
    RfOff();
    /* Same setup as normal reception, then FM so the sweep ignores any AM setting. */
    Rfic_ConfigRxMode();
    Rfic_SwitchFM_AM(ModeFM);
    SpeakerSwitch(OFF);

    g_sysRunPara.sysRunMode = MODE_SPECTRUM;
    spCenter = g_CurrentVfo->rx->frequency;
    spSpanIdx = 2;
    spLastBand = 0xFF; /* RfOff above cut the RX path, force it back on. */
    SpClampCenter();

    SpDraw(0, 0, 0, 0);
    fresh = TRUE;

    savedAgc = Rfic_ReadWord(SP_AGC_REG);
    Rfic_WriteWord(SP_AGC_REG, (savedAgc & SP_AGC_MASK) | SP_AGC_FIXED);
    DelayMs(SP_START_WAIT);

    while(1)
    {
        start = spCenter - spSpanTab[spSpanIdx] / 2;
        step = spSpanTab[spSpanIdx] / SP_COLS;
        action = SP_KEEP_GOING;
        peakCol = 0;
        minVal = 0xFF;
        maxVal = 0;

        for(col = 0; col < SP_COLS; col += SP_COLS_PER_POINT)
        {
            action = SpService();
            if(action != SP_KEEP_GOING)
            {
                break;
            }

            SpTune(start + step * col);
            DelayMs(SP_SETTLE_MS);

            reading = Rfic_GetRssiVal();
            spRssi[col] = (reading > 0xFF) ? 0xFF : (U8)reading;
            spRssi[col + 1] = spRssi[col];
            /* Later sweeps keep the last full picture until they finish, to avoid flicker. */

            if(spRssi[col] < minVal)
            {
                minVal = spRssi[col];
            }
            if(spRssi[col] > maxVal)
            {
                maxVal = spRssi[col];
                peakCol = col;
            }
            if(fresh && (col + SP_COLS_PER_POINT) % SP_REDRAW_COLS == 0)
            {
                SpDraw(col + SP_COLS_PER_POINT, peakCol, minVal, maxVal);
            }
        }

        if(action == SP_EXIT)
        {
            break;
        }
        if(action == SP_PEAK)
        {
            if(spSpanIdx > 0)
            {
                /* Too wide to search in one go: first closer, keeping the signal in view. */
                spCenter = spShownPeak;
                if(spHasPeak)
                {
                    spCenter -= spSpanTab[spSpanIdx] / SP_ZOOM_SHIFT_DIV;
                }
                spSpanIdx--;
                SpClampCenter();
            }
            else
            {
                SpListen(spHasPeak ? SpFindExact(spShownPeak) : spShownPeak, savedAgc);
                DelayMs(SP_START_WAIT);
            }
            action = SP_RESTART;
        }
        if(action == SP_KEEP_GOING)
        {
            SpDraw(SP_COLS, peakCol, minVal, maxVal);
            fresh = FALSE;
        }
        else
        {
            /* A key changed the span or centre: show it now, without waiting for a sweep. */
            SpDraw(0, 0, 0, 0);
            fresh = TRUE;
        }
    }

    Rfic_WriteWord(SP_AGC_REG, savedAgc);
    g_sysRunPara.sysRunMode = MODE_MAIN;
    RfOff();
    /* RX_READY makes RF_Task run Rfic_ConfigRxMode for the current VFO. */
    RxReset();
    DisplayHomePage();
    BeepOut(BEEP_EXITMENU);
}
