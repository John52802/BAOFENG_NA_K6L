#include "includes.h"

extern void UI_DisplayPowerOn(void)
{
    U8 i;
    U16 wait;
    U8 picDisBuf[1024+1] = {0};

    SC5620_SetContpastRatio(g_radioInform.brightness);

    if(g_radioInform.OpFlag1.Bit.b0 == 0)
    {
        SpiFlash_ReadBytes(FLASH_PON_MSG_ADDR,picDisBuf,1024);
        SC5260_DisplayArea(0,0,128,64,picDisBuf,LCD_DIS_NORMAL);   
    }
    else if(g_radioInform.OpFlag1.Bit.b0 == 1)
    {
        memset(picDisBuf,' ',16);
        for(i=0;i<16;i++)
        {
            if(powerOnMsg[i] == 0xFF || powerOnMsg[i] == 0x00)
            {
               break;
            }
            picDisBuf[i] = powerOnMsg[i];
        }
        LCD_DisplayText(24,(64-i*4),(U8 *)picDisBuf, FONTSIZE_16x16,LCD_DIS_NORMAL);
    }
    else
    {
        DisplayBatteryVol(0);
    }

    /* Everything above only draws into the RAM buffer: this is what puts it on the screen. */
    LCD_UpdateFullScreen();
    LcdBackLightSwitch(LED_ON);
    //延时500ms
    /* Long enough to see the logo or message; the battery voltage screen keeps its short wait.
       Counted on the 1 ms tick, because the compiler removes the empty loop inside DelayMs. */
    wait = (g_radioInform.OpFlag1.Bit.b0 <= 1) ? 2000 : 500;
    while(wait)
    {
        if(g_msFlag)
        {
            g_msFlag = FALSE;
            wait--;
        }
    }
}

