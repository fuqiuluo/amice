@interface TestClass
{
    int value;
}
- (void)setValue:(int)v;
- (int)getValue;
@end

@implementation TestClass
- (void)setValue:(int)v {
    value = v;
}
- (int)getValue {
    return value;
}
@end

@interface SecretClass
{
    int secret;
}
- (void)setSecret:(int)s;
- (int)getSecret;
@end

@implementation SecretClass
- (void)setSecret:(int)s {
    secret = s;
}
- (int)getSecret {
    return secret;
}
@end

int main() {
    return 0;
}