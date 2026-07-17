public class PrimArr {
    public static void main(String[] a){
        boolean[] b=new boolean[3]; b[1]=true;
        byte[] by=new byte[3]; by[0]=(byte)200;      // → -56
        char[] c=new char[2]; c[0]='Z';
        long[] l=new long[3]; l[0]=9000000000L; l[1]=l[0]*2;
        double[] d=new double[2]; d[0]=3.5; d[1]=d[0]*2;
        System.out.println(b[1]+" "+by[0]+" "+(int)c[0]+" "+l[1]+" "+d[1]);
    }
}
