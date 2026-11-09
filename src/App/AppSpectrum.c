#include "includes.h"

/*
 * Receive-only spectrum view.
 *
 * Runs its own loop instead of the main task loop. PTT_ScanTask and RF_Task are
 * never called from here, so the radio cannot enter TX while this mode is open.
 *
 * Keys: UP/DOWN move the centre, * narrows the span, # widens it, EXIT or MENU leaves.
 * Frequencies are in units of 10 Hz, matching the rest of the firmware.
 */

extern U8 gLcdBuffer[8][128];
extern void Rfic_BandInitial(U32 freq);

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
#define SP_RX_MIN      (RX_MIN_100KHZ * 10000UL)
#define SP_RX_MAX      (RX_MAX_100KHZ * 10000UL)

enum { SP_KEEP_GOING = 0, SP_RESTART, SP_EXIT };

static const U32 spSpanTab[] = { 200000UL, 500000UL, 1000000UL, 2000000UL, 5000000UL };
#define SP_SPAN_CNT    (sizeof(spSpanTab) / sizeof(spSpanTab[0]))

static U32 spCenter;
static U8  spSpanIdx;
static U8  spRssi[SP_COLS];
static U8  spLastBand;

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
        case KEYID_MENU:
            return SP_EXIT;
        default:
            return SP_KEEP_GOING;
    }

    SpClampCenter();
    return SP_RESTART;
}

/* Keeps keys, backlight and the power switch alive during a sweep. */
static U8 SpService(void)
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

static void SpDraw(U8 peakCol, U8 minVal, U8 maxVal)
{
    String text[32];
    U32 span = spSpanTab[spSpanIdx];
    U32 start = spCenter - span / 2;
    U32 peak = start + (span / SP_COLS) * peakCol;
    U16 range = maxVal - minVal;
    U8 col, y, height;

    if(range < SP_MIN_RANGE)
    {
        range = SP_MIN_RANGE;
    }

    LCD_ClearWorkArea();

    sprintf(text, "%3lu.%03lu ~%3lu.%03lu %2luM",
            spCenter / 100000UL, (spCenter % 100000UL) / 100UL,
            peak / 100000UL, (peak % 100000UL) / 100UL,
            span / 100000UL);
    LCD_DisplayText(9, 0, (U8 *)text, FONTSIZE_12x12, LCD_DIS_NORMAL);

    for(col = 0; col < SP_COLS; col++)
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
    if(maxVal - minVal < 2)
    {
        sprintf(text, "RSSI %u", maxVal);
        LCD_DisplayText(36, 40, (U8 *)text, FONTSIZE_12x12, LCD_DIS_NORMAL);
    }

    LCD_UpdateWorkAre();
}

extern void EnterSpectrumMode(void)
{
    U8  col, action, peakCol, minVal, maxVal;
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

    LCD_ClearWorkArea();
    LCD_UpdateWorkAre();

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

            if(spRssi[col] < minVal)
            {
                minVal = spRssi[col];
            }
            if(spRssi[col] > maxVal)
            {
                maxVal = spRssi[col];
                peakCol = col;
            }
        }

        if(action == SP_EXIT)
        {
            break;
        }
        if(action == SP_KEEP_GOING)
        {
            SpDraw(peakCol, minVal, maxVal);
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
