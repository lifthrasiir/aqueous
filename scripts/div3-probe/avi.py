import struct, subprocess, os, tempfile
def chunk(i,d): return i+struct.pack('<I',len(d))+d+(b'\0' if len(d)&1 else b'')
def lst(t,d): return b'LIST'+struct.pack('<I',len(d)+4)+t+d
def make_avi(frames,w,h):
    avih=struct.pack('<14I',41708,0,0,0x10,len(frames),0,1,1<<20,w,h,0,0,0,0)
    strh=b'vids'+b'div3'+struct.pack('<IHHIIIIIIIIhhhh',0,0,0,0,1001,24000,0,len(frames),1<<20,0xffffffff,0,0,0,w,h)
    strf=struct.pack('<IiiHH4sIiiII',40,w,h,1,24,b'DIV3',w*h*3,0,0,0,0)
    hdrl=lst(b'hdrl',chunk(b'avih',avih)+lst(b'strl',chunk(b'strh',strh)+chunk(b'strf',strf)))
    movi=lst(b'movi',b''.join(chunk(b'00dc',f) for f in frames))
    body=b'AVI '+hdrl+movi
    return b'RIFF'+struct.pack('<I',len(body))+body
def decode(frames,w,h,extra=()):
    d=tempfile.mkdtemp(); p=os.path.join(d,'a.avi'); open(p,'wb').write(make_avi(frames,w,h))
    r=subprocess.run(['ffmpeg','-hide_banner','-v','error','-i',p,'-vsync','0','-f','rawvideo','-pix_fmt','yuv420p','-',*extra],capture_output=True)
    return r.stdout,r.stderr.decode(errors='replace')
