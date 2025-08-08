#include <stdio.h>
#include <stdlib.h>
#include <time.h>

int main() {
    int final_result = 0;

    for (int i = 0; i < 4; i++) {
        int temp = i;

        switch (temp % 4) {
            case 0:
                final_result += 1;
                break;
            case 1:
                final_result -=2;
                break;
            case 2:
                final_result *= (temp == 0) ? 1 : temp;
                break;
        }

        printf("当前结果: %d -> %d\n", i, final_result);
    }

//    if(final_result == 0) {
//        final_result = 999;
//    }

    printf("\n最终结果: %d\n", final_result);
    printf("测试完成！\n");

    return 0;
}

//综合测试:
//当前结果: 1
//当前结果: -1
//当前结果: -2
//当前结果: 0
//
//最终结果: 0
//测试完成！