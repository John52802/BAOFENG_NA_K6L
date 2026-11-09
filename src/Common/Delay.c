#include "includes.h"

void DelayUs(U16 n)// 1US
{
    /* Volatile: otherwise the compiler removes this empty loop and every delay waits nothing. */
    volatile U16 t = n * 8;
    while(t--){
	
    }
}
void DelayMs(U16 n)
{
   U16 i=0;
   for (i=0;i<n;i++)
	{
		DelayUs( 1000 );
	}
}
