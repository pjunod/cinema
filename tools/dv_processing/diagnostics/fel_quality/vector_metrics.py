"""Vectorized BT.2124 scoring of exact RGB samples retained by remote_probe.py."""
import importlib.util,json,math,pathlib,sys
import numpy as np
M1=2610/16384;M2=2523/32;C1=3424/4096;C2=2413/128;C3=2392/128
A=np.array(((1688,2146,262),(683,2951,462),(99,309,3688)),dtype=np.float64)/4096
B=np.array(((2048,2048,0),(6610,-13613,7003),(17933,-17390,-543)),dtype=np.float64)/4096

def ictcp(rgb):
    x=np.asarray(rgb,dtype=np.float64)/65535;p=x**(1/M2)
    linear=10000*(np.maximum(p-C1,0)/(C2-C3*p))**(1/M1)
    lms=linear@A.T;power=(lms/10000)**M1
    return (((C1+C2*power)/(1+C3*power))**M2)@B.T

def delta(a,b):
    d=ictcp(a)-ictcp(b);d[...,1]*=.5
    return 720*np.sqrt(np.sum(d*d,axis=-1))

def stats(values):
    x=np.asarray(values).ravel()
    if not len(x):return None
    return {'count':len(x),'mean':float(x.mean()),'p95':float(np.partition(x,int(.95*(len(x)-1)))[int(.95*(len(x)-1))]),'p99':float(np.partition(x,int(.99*(len(x)-1)))[int(.99*(len(x)-1))]),'max':float(x.max())}

def psnr(a,b,mask=None):
    d=(np.asarray(a,dtype=np.float64)-b)/65535
    if mask is not None:d=d[mask]
    mse=float(np.mean(d*d));return None if mse==0 else -10*math.log10(mse)

