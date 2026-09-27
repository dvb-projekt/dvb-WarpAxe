import sys
from PIL import Image
raw=open(sys.argv[1],'rb').read(); out=sys.argv[2]
if len(raw)==320*170*2:
    im=Image.new('RGB',(320,170))
    px=[]
    for i in range(0,len(raw),2):
        v=raw[i]<<8|raw[i+1]; px.append((((v>>11)&31)*255//31,((v>>5)&63)*255//63,(v&31)*255//31))
    im.putdata(px); im=im.resize((960,510),Image.NEAREST)
else:
    im=Image.new('L',(128,32))
    for p in range(4):
        for x in range(128):
            b=raw[p*128+x]
            for r in range(8):
                im.putpixel((x,p*8+r),230 if b>>r&1 else 0)
    im=im.resize((512,128),Image.NEAREST)
im.save(out)
